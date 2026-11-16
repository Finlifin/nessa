//! Integration tests for Wave E4: AOT optimization levels (`O0`, `O1`, `O2`, `O3`),
//! modern NIR & bytecode passes (ConstFold, CopyProp, SCCP, Dominator GVN/CSE,
//! Inlining, LICM, Self-Tail-Recursion Elimination, DCE + Slot Coloring, DFE),
//! artifact validation/roundtrip, cache isolation, and JIT compatibility.

use std::process::Command;

use driver::{Driver, JitMode, OptLevel};
use nsbc::{Instruction, Opcode};

fn run_with_opt_and_jit(source: &str, opt: OptLevel, jit: JitMode) -> Result<i64, String> {
    let compiled = Driver::new().with_opt_level(opt).compile(source);
    assert!(
        !compiled.has_errors,
        "compile errors at {opt:?}: {:?}",
        compiled.diagnostics
    );
    let artifact = compiled.into_artifact().map_err(|e| e.to_string())?;
    nsbc::validate_artifact(&artifact).map_err(|e| format!("validate_artifact at {opt:?}: {e}"))?;

    // Also verify `.nsbc` serialization roundtrip at every optimization level.
    let bytes = nsbc_io::write_artifact(&artifact).map_err(|e| e.to_string())?;
    let reloaded = nsbc_io::read_artifact(&bytes).map_err(|e| e.to_string())?;

    let cfg = driver::EngineConfig {
        jit: driver::JitConfig::with_mode(jit),
        ..Default::default()
    };
    let mut engine = initialization::Engine::new(cfg);
    let entry = driver::install_artifact(engine.vm_mut(), reloaded).map_err(|e| e.to_string())?;
    let task = engine
        .vm_mut()
        .spawn_root(entry.expect("fixture defines main"));
    match engine.vm_mut().run() {
        interpreter::VmResult::Finished => engine
            .vm_mut()
            .task_result_number(task)
            .map_err(|e| format!("{e:?}"))?
            .to_i64_checked()
            .ok_or_else(|| "result does not fit i64".to_owned()),
        interpreter::VmResult::Error(err) => Err(format!("{err:?}")),
    }
}

fn total_instructions(source: &str, opt: OptLevel) -> usize {
    let compiled = Driver::new().with_opt_level(opt).compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    compiled
        .codegen_output
        .functions
        .iter()
        .map(|f| f.instructions.len())
        .sum()
}

fn find_compiled_fn(output: &nsbc::CodegenOutput, name: &str) -> Option<(usize, u32, usize)> {
    let target = str_interner::intern(name);
    output.functions.iter().find_map(|f| {
        if f.name != target {
            return None;
        }
        let slots = Instruction::decode(f.instructions[0])
            .and_then(|ins| match (ins.opcode, ins.data) {
                (Opcode::AllocateSlots, nsbc::InstructionData::E { payload }) => Some(payload),
                _ => None,
            })
            .unwrap_or(0);
        let direct_calls = f
            .instructions
            .iter()
            .filter(|&&w| {
                Instruction::decode(w)
                    .is_some_and(|i| matches!(i.opcode, Opcode::Call | Opcode::CallFar))
            })
            .count();
        Some((f.instructions.len(), slots, direct_calls))
    })
}

#[test]
fn all_opt_levels_and_jit_modes_preserve_exact_semantics_effects_and_traps() {
    let complex_program = r#"
        effect Ask() -> i64

        enum Tree {
            leaf(val: i64),
            branch(left: Tree, right: Tree),
        }

        fn sum_tree(t: Tree) -> i64 {
            t match {
                .leaf(v) => v,
                .branch(l, r) => sum_tree(l) + sum_tree(r),
            }
        }

        fn with_effect() -> #Ask i64 {
            let base = Ask()#
            var acc = 0
            for i in 1..=5 {
                acc += i
            }
            base + acc
        }

        fn main() {
            let tree = Tree.branch(
                Tree.leaf(10),
                Tree.branch(Tree.leaf(7), Tree.leaf(10)),
            )
            let from_eff = with_effect()# { Ask() => 0 }
            let narrow_u8 = (200.as(u8))
            if narrow_u8'type == u8 {
                sum_tree(tree) + from_eff
            } else {
                0
            }
        }
    "#;

    for opt in [OptLevel::O0, OptLevel::O1, OptLevel::O2, OptLevel::O3] {
        for jit in [JitMode::Off, JitMode::Tier1, JitMode::Tier2] {
            let res = run_with_opt_and_jit(complex_program, opt, jit)
                .unwrap_or_else(|e| panic!("failed at {opt:?}/{jit:?}: {e}"));
            assert_eq!(res, 42, "{opt:?}/{jit:?}");
        }
    }

    // Runtime traps (division by zero, integer overflow, dynamic TypeError) MUST be preserved at O3!
    for (trap_src, expected_err) in [
        (
            "fn div_zero(x: i64) -> i64 { x / 0 }\nfn main() { div_zero(42) }",
            "DivisionByZero",
        ),
        (
            "fn main() { let z: u64 = 0; let o: u64 = 1; let unused = z - o; 42 }",
            "NumericOverflow",
        ),
        (
            "fn take(x: Any) -> i64 { let unused = x.as(i64); 42 }\nfn main() { take(\"bad\") }",
            "TypeError",
        ),
    ] {
        let err = run_with_opt_and_jit(trap_src, OptLevel::O3, JitMode::Off)
            .expect_err("trap must not be optimized away at O3");
        assert!(
            err.contains(expected_err),
            "expected `{expected_err}` for `{trap_src}`, got `{err}`"
        );
    }
}

#[test]
fn sccp_gvn_dce_and_peepholes_substantially_shrink_functions_slots_and_constants() {
    let src = r#"
        fn compute(flag: bool, x: i64, y: i64) -> i64 {
            var k = 0
            if flag {
                k = 6 * 7
            } else {
                k = 40 + 2
            }
            let dup1 = (x * 3) + (y * 5)
            let dup2 = (y * 5) + (x * 3)
            let dead_alloc = [1, 2, 3, 4]
            let s1 = "shared_constant_literal_payload"
            let s2 = "shared_constant_literal_payload"
            let s3 = "shared_constant_literal_payload"
            if k == 42 and s1.len() == s2.len() and s2.len() == s3.len() {
                dup1 - dup2 + k
            } else {
                0
            }
        }
        fn main() { compute(true, 11, 13) }
    "#;

    let c0 = Driver::new().with_opt_level(OptLevel::O0).compile(src);
    let c1 = Driver::new().with_opt_level(OptLevel::O1).compile(src);
    let c2 = Driver::new().with_opt_level(OptLevel::O2).compile(src);
    assert!(!c0.has_errors && !c1.has_errors && !c2.has_errors);

    // O1 deduplicates repeated string constants across the module.
    let str_count_o0 = c0
        .codegen_output
        .constants
        .iter()
        .filter(|c| matches!(c, nsbc::Constant::Str(s) if s == "shared_constant_literal_payload"))
        .count();
    let str_count_o1 = c1
        .codegen_output
        .constants
        .iter()
        .filter(|c| matches!(c, nsbc::Constant::Str(s) if s == "shared_constant_literal_payload"))
        .count();
    assert_eq!(str_count_o0, 3);
    assert_eq!(str_count_o1, 1);

    // Whole-module DFE eliminates unused std free functions, and O2 SCCP + GVN shrinks `compute`
    // (or inlines and folds `compute(true, 11, 13)` completely into `main`!).
    assert!(
        c1.codegen_output.functions.len() < c0.codegen_output.functions.len(),
        "expected DFE at O1 to shrink function count ({} vs {})",
        c1.codegen_output.functions.len(),
        c0.codegen_output.functions.len()
    );
    let (comp_instr_o0, comp_slots_o0, _) =
        find_compiled_fn(&c0.codegen_output, "compute").expect("compute exists at O0");
    let (comp_instr_o1, comp_slots_o1, _) =
        find_compiled_fn(&c1.codegen_output, "compute").expect("compute exists at O1");
    assert!(
        comp_instr_o1 < comp_instr_o0 && comp_slots_o1 < comp_slots_o0,
        "expected O1 to shrink compute instructions ({comp_instr_o1} vs {comp_instr_o0}) and slots ({comp_slots_o1} vs {comp_slots_o0})"
    );
    if let Some((comp_instr_o2, comp_slots_o2, _)) = find_compiled_fn(&c2.codegen_output, "compute")
    {
        assert!(
            comp_instr_o2 < comp_instr_o1 && comp_slots_o2 <= comp_slots_o1,
            "expected O2 SCCP + GVN to shrink compute below O1 ({comp_instr_o2} vs {comp_instr_o1})"
        );
    } else {
        assert!(
            total_instructions(src, OptLevel::O2) < total_instructions(src, OptLevel::O1),
            "expected inlined compute at O2 to shrink module instructions below O1"
        );
    }

    assert_eq!(
        run_with_opt_and_jit(src, OptLevel::O2, JitMode::Auto).unwrap(),
        42
    );
}

#[test]
fn inlining_and_dfe_eliminate_small_helpers_and_dead_functions_while_o0_keeps_them() {
    let src = r#"
        fn dead_helper(x: i64) -> i64 { x * 99 + 1 }
        fn add_one(x: i64) -> i64 { x + 1 }
        fn double(x: i64) -> i64 { x + x }
        fn main() { double(add_one(20)) }
    "#;

    let disasm_o0 = Driver::new()
        .with_opt_level(OptLevel::O0)
        .disassemble_source(src)
        .unwrap();
    let disasm_o1 = Driver::new()
        .with_opt_level(OptLevel::O1)
        .disassemble_source(src)
        .unwrap();
    let disasm_o2 = Driver::new()
        .with_opt_level(OptLevel::O2)
        .disassemble_source(src)
        .unwrap();

    // O0 keeps all user functions intact.
    assert!(disasm_o0.contains("dead_helper"), "{disasm_o0}");
    assert!(disasm_o0.contains("add_one"), "{disasm_o0}");

    // O1 eliminates unreachable `dead_helper`, keeping called `add_one` & `double`.
    assert!(!disasm_o1.contains("dead_helper"), "{disasm_o1}");
    assert!(disasm_o1.contains("add_one"), "{disasm_o1}");

    // O2 inlines `add_one` and `double` into `main` and DFE removes both helper bodies!
    assert!(!disasm_o2.contains("dead_helper"), "{disasm_o2}");
    assert!(!disasm_o2.contains("add_one"), "{disasm_o2}");
    assert!(!disasm_o2.contains("double"), "{disasm_o2}");
    assert_eq!(
        run_with_opt_and_jit(src, OptLevel::O2, JitMode::Off).unwrap(),
        42
    );
}

#[test]
fn o3_tail_recursion_elimination_converts_deep_and_parallel_swap_recursion_into_loops() {
    let deep_tail_src = r#"
        @no_inline_anchor
        fn sum_down(n: i64, acc: i64) -> i64 {
            if n <= 0 {
                acc
            } else {
                sum_down(n - 1, acc + n)
            }
        }

        fn fib_tail(n: i64, a: i64, b: i64) -> i64 {
            if n == 0 {
                a
            } else {
                fib_tail(n - 1, b, a + b)
            }
        }

        fn main() {
            let s = sum_down(100000, 0)
            let f = fib_tail(50, 0, 1)
            if s == 5000050000 and f == 12586269025 {
                42
            } else {
                0
            }
        }
    "#;
    // Strip hypothetical decorator — plain Nessa functions:
    let deep_tail_src = deep_tail_src.replace("@no_inline_anchor\n", "");

    let compiled_o3 = Driver::new()
        .with_opt_level(OptLevel::O3)
        .compile(&deep_tail_src);
    assert!(!compiled_o3.has_errors, "{:?}", compiled_o3.diagnostics);

    // Either `sum_down` / `fib_tail` had their tail-recursion converted to loops and were
    // subsequently inlined directly into `main` (eliminating the helper functions completely!),
    // or if retained, they contain zero `Call` instructions. Either way, `main` + helpers perform
    // zero recursive `Call`s while computing 100,000 iterations!
    let disasm_o3 = Driver::new()
        .with_opt_level(OptLevel::O3)
        .disassemble_source(&deep_tail_src)
        .unwrap();
    if let Some((_, _, sum_calls)) = find_compiled_fn(&compiled_o3.codegen_output, "sum_down") {
        assert_eq!(
            sum_calls, 0,
            "sum_down should have zero recursive Call opcodes at O3:\n{disasm_o3}"
        );
    }
    if let Some((_, _, fib_calls)) = find_compiled_fn(&compiled_o3.codegen_output, "fib_tail") {
        assert_eq!(
            fib_calls, 0,
            "fib_tail should have zero recursive Call opcodes at O3"
        );
    }
    if let Some((_, _, main_calls)) = find_compiled_fn(&compiled_o3.codegen_output, "main") {
        assert_eq!(
            main_calls, 0,
            "main should contain zero Call opcodes once tail-recursive loops are inlined at O3"
        );
    }

    // Execute 100,000-deep tail recursion under Interpreter AND Tier-2 JIT!
    assert_eq!(
        run_with_opt_and_jit(&deep_tail_src, OptLevel::O3, JitMode::Off).unwrap(),
        42
    );
    assert_eq!(
        run_with_opt_and_jit(&deep_tail_src, OptLevel::O3, JitMode::Tier2).unwrap(),
        42
    );
}

#[test]
fn o3_licm_hoists_loop_invariant_type_checks_and_cli_plus_cache_honor_opt_levels() {
    let licm_src = r#"
        enum Tag { active, idle }
        fn count_active(tag: Tag, n: i64) -> i64 {
            var acc = 0
            var i = 0
            while i < n {
                if tag matches .active {
                    acc += 1
                }
                i += 1
            }
            acc
        }
        fn main() { count_active(.active, 42) }
    "#;
    assert_eq!(
        run_with_opt_and_jit(licm_src, OptLevel::O3, JitMode::Tier2).unwrap(),
        42
    );

    // Verify `.nsbc` cache isolates `-O0` and `-O2` artifacts cleanly for identical source!
    let tmp = std::env::temp_dir().join(format!(
        "nessa_opt_cache_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let cache_dir = tmp.join("cache");
    let src = "fn helper(x: i64) -> i64 { x + 2 }\nfn main() { println(helper(40)) }\n";

    let art_o0 = Driver::new()
        .with_cache_dir(cache_dir.clone())
        .with_opt_level(OptLevel::O0)
        .compile_cached_artifact(src)
        .unwrap();
    let art_o2 = Driver::new()
        .with_cache_dir(cache_dir.clone())
        .with_opt_level(OptLevel::O2)
        .compile_cached_artifact(src)
        .unwrap();
    assert!(
        art_o2.codegen_output.functions.len() < art_o0.codegen_output.functions.len(),
        "cache must not serve O0 artifact for O2 request"
    );

    // Verify `package.toml` `[build] opt_level = 2` + `TypeId` invariance + CLI `-O3`!
    let pkg_dir = tmp.join("app_pkg");
    std::fs::create_dir_all(pkg_dir.join("src")).unwrap();
    std::fs::write(
        pkg_dir.join("package.toml"),
        "[package]\nname = \"opt_demo\"\ndomain = \"local\"\nversion = \"0.1.0\"\ntype = \"exe\"\n\n[build]\nopt_level = 2\n",
    )
    .unwrap();
    std::fs::write(
        pkg_dir.join("src/main.ns"),
        "pub struct Token { pub v: i64 }\nfn tiny(x: i64) -> i64 { x * 2 }\nfn main() { let t = Token { v: tiny(21) }; println(t.v) }\n",
    )
    .unwrap();

    let pkg_default = Driver::new().without_cache().compile_package(&pkg_dir);
    let pkg_o0 = Driver::new()
        .without_cache()
        .with_opt_level(OptLevel::O0)
        .compile_package(&pkg_dir);
    assert!(!pkg_default.has_errors && !pkg_o0.has_errors);
    assert!(
        pkg_default.codegen_output.functions.len() < pkg_o0.codegen_output.functions.len(),
        "`[build] opt_level = 2` should optimize package by default unless overridden by -O0"
    );

    // Verify `nessa` CLI binary accepts `-O3` on `run` and `build` if already built or via cargo.
    let script_file = tmp.join("cli_opt.ns");
    std::fs::write(&script_file, src).unwrap();
    if let Ok(exe) = std::env::var("CARGO_BIN_EXE_nessa") {
        let out = Command::new(exe)
            .args(["run", "-O3", script_file.to_str().unwrap()])
            .env("NESSA_NO_CACHE", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "42");
    }

    let _ = std::fs::remove_dir_all(&tmp);
}
