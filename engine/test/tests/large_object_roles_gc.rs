//! End-to-end integration tests for `PayloadRole::LargeTagged` and `PayloadRole::ExternalRc`
//! under real MMTk Immix + LOS collections, external memory pressure accounting, zero-copy
//! slicing, and multi-shot delimited continuations (`k.clone()`).

#[test]
fn large_tagged_containers_external_rc_strings_bytes_and_continuations_survive_gc() {
    let source = r#"
        use std.bytes.{Bytes}
        use std.collections.{HashMap, Set}
        use std.runtime

        effect fork_check(seed: i64, catch k) -> i64

        struct BranchRunner {}

        impl BranchRunner {
            pub fn split(self, seed: i64, catch k) -> i64 handles(fork_check) {
                let first = k.clone()(seed + 1)
                let _ = runtime.collect_garbage()
                let second = k(seed + 2)
                first + second
            }
        }

        fn build_and_detach_small_slice() -> String {
            let big = "0123456789abcdef".repeat(131072) -- 2,097,152 bytes (2 MiB)
            if big.len() != 2097152 {
                return "ERR_LEN"
            }
            let shared_sub = big.slice(16, 1500000) -- > 1/4 capacity -> O(1) shared ExternalRc
            if shared_sub.len() != 1499984 or shared_sub.slice(0, 6) != "012345" {
                return "ERR_SHARED"
            }
            big.slice(4, 14) -- 10 bytes -> compact inline Immix String!
        }

        fn run_multishot_inner(big_list: List) -> #fork_check i64 {
            let live_ext = "M".repeat(900000)
            let delta = fork_check(10)#
            let _ = runtime.collect_garbage()
            if big_list.get(0).as(String) != "head_item_0" or big_list.get(74999).as(String) != "tail_item_74999" or live_ext.len() != 900000 {
                return -900
            }
            delta
        }

        fn run_multishot_branch(big_list: List) -> i64 {
            handles(fork_check) let runner = BranchRunner {}
            run_multishot_inner(big_list)#
        }

        fn main() -> i64 {
            -- 1. Churn 10 x 4 MiB = 40 MiB of ephemeral ExternalRc strings & Bytes inside a
            --    32 MiB MMTk Immix heap! External memory pressure accounting must trigger
            --    safepoint collections automatically without exhausting the 32 MiB heap.
            var r: i64 = 0
            while r < 10 {
                let tmp_s = "abcd".repeat(1048576)
                let tmp_b = Bytes.with_capacity(4194304)
                if tmp_s.len() != 4194304 or tmp_b.capacity() != 4194304 {
                    return -1
                }
                r += 1
            }

            -- 2. Large `ExternalRc` String (> 512 KiB legacy limit): 2 MiB text + O(1) large slice
            --    sharing + compact inline small-slice detachment.
            let small_slice = build_and_detach_small_slice()
            let _ = runtime.collect_garbage()
            if small_slice != "456789abcd" {
                return -4
            }

            -- 3. Large `ExternalRc` Bytes (> 8 KiB threshold, 1.5 MiB) with COW slice & mutation.
            let b0 = Bytes.from_string("XYZ_".repeat(393216)) -- 1,572,864 bytes
            let b_view = b0.slice(4, 1000000)
            b0.set(4, 65) -- COW detach on 1.5 MiB ExternalRc buffer!
            let _ = runtime.collect_garbage()
            if b0.get(4) != 65 or b_view.get(0) != 88 {
                return -5
            }

            -- 4. `LargeTagged` List (`75_000` elements > `65_535` u16::MAX) holding heap Strings
            --    and surviving moving Immix GC + multi-shot `k.clone()` continuation!
            let big_list = List()
            var i: i64 = 0
            while i < 75000 {
                if i == 0 {
                    big_list.push("head_item_" ++ i.to_string())
                } else if i == 74999 {
                    big_list.push("tail_item_" ++ i.to_string())
                } else {
                    big_list.push(i)
                }
                i += 1
            }
            if big_list.len() != 75000 {
                return -6
            }

            -- 5. `LargeTagged` String-keyed `Map` (`10_000` entries > `8_192` legacy bucket cap ->
            --    `16_384` buckets = `65_536` words > `u16::MAX`!) + std `HashMap` & `Set`.
            let m = Map()
            var mi: i64 = 0
            while mi < 10000 {
                m.insert("k_" ++ mi.to_string(), mi * 3)
                mi += 1
            }
            if m.len() != 10000 or m.get("k_9999").as(i64) != 29997 {
                return -7
            }

            let hm = HashMap.new()
            let st = Set.new()
            hm.insert("big_str", "Q".repeat(700000))
            st.insert(75000)
            if hm.get("big_str").as(String).len() != 700000 or (not st.contains(75000)) {
                return -8
            }

            -- 6. Multi-shot continuation `k.clone()` capturing live `LargeTagged` List & `ExternalRc` String!
            let branch_val = run_multishot_branch(big_list)
            -- `split` computes `(10 + 1) + (10 + 2) = 23`; add `19` -> `42`!
            branch_val + 19
        }
    "#;

    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);

    let mut engine = initialization::Engine::new(initialization::EngineConfig {
        gc: gc::GcConfig {
            heap_size: 32 * 1024 * 1024,
            plan: "Immix".into(),
            gc_threads: 2,
        },
        debug: false,
        ..Default::default()
    });
    let entry = driver::install_artifact(engine.vm_mut(), compiled.into_artifact().unwrap())
        .unwrap()
        .unwrap();
    let before_gc = engine.vm_mut().completed_collections();
    let task = engine.vm_mut().spawn_root(entry);
    let result = engine.vm_mut().run();
    assert!(
        matches!(result, interpreter::VmResult::Finished),
        "{result:?}"
    );
    assert!(
        engine.vm_mut().completed_collections() > before_gc,
        "external memory pressure + runtime.gc() triggered real MMTk collections"
    );
    assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
    assert_eq!(engine.vm_mut().active_stack_count(), 0);

    // Drop the engine and run a collection on a fresh handle to ensure all unreachable
    // ExternalRc shells from the finished VM are swept to 0 bytes!
    drop(engine);
    let mut verifier = initialization::Engine::with_defaults();
    assert!(verifier.vm.collect_garbage().unwrap());
    assert_eq!(
        gc::external_live_bytes(),
        0,
        "all off-heap ExternalRc buffers reclaimed to 0 after VM drop & GC"
    );
}
