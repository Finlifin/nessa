//! End-to-end integration tests for the tiered hot-loop & function JIT (`engine/jit`):
//! - Differential equivalence across `Off`, `Tier1`, `Tier2`, and `Auto`
//! - Single-call loop back-edge OSR rescue (`main` invoked once with a 5,000,000-iteration loop)
//! - Tier-2 mid-loop 57-bit integer overflow deoptimization into 64-bit / 128-bit promotion
//! - Hybrid `f64` (`IMM_F64` + heap-boxed `F64`), struct/enum/tuple ICs, closures, `!Error`,
//!   async coroutines, multi-shot `Continuation.clone()`, low-level NSBC bitwise opcodes,
//!   and code-cache cap graceful fallback.

use std::time::Instant;

use driver::{Driver, JitConfig, JitMode, JitStats};
use initialization::{Engine, EngineConfig};
use interpreter::VmResult;
use nsbc::{FuncId, FunctionAbi, Instruction, Opcode, Reg};
use runtime::{FunctionCode, Number};
use type_pool::TypeIndex;

const ALL_MODES: [JitMode; 4] = [JitMode::Off, JitMode::Tier1, JitMode::Tier2, JitMode::Auto];

fn compile_to_bytes(source: &str) -> Vec<u8> {
    let compiled = Driver::new().without_cache().compile(source);
    assert!(
        !compiled.has_errors,
        "compile errors: {:?}",
        compiled.diagnostics
    );
    let artifact = compiled.into_artifact().expect("valid artifact");
    nsbc_io::write_artifact(&artifact).expect("archive serializes cleanly")
}

fn run_with_config(
    archive_bytes: &[u8],
    jit: JitConfig,
) -> (Result<Number, String>, JitStats, bool) {
    let artifact = nsbc_io::read_artifact(archive_bytes).expect("archive loads cleanly");
    let mut engine = Engine::new(EngineConfig {
        jit,
        ..Default::default()
    });
    let enabled = engine.vm.jit_enabled();
    let entry = driver::install_artifact(engine.vm_mut(), artifact)
        .expect("artifact installs cleanly")
        .expect("fixture defines main");
    let task = engine.vm_mut().spawn_root(entry);
    let outcome = match engine.vm_mut().run() {
        VmResult::Finished => {
            assert_eq!(engine.vm.active_stack_count(), 0);
            engine
                .vm_mut()
                .task_result_number(task)
                .map_err(|err| format!("{err:?}"))
        }
        VmResult::Error(err) => Err(format!("{err:?}")),
    };
    let stats = engine.vm.jit_stats();
    (outcome, stats, enabled)
}

fn assert_same_number_or_err(
    left: &Result<Number, String>,
    right: &Result<Number, String>,
    mode: JitMode,
) {
    match (left, right) {
        (Ok(Number::F64(a)), Ok(Number::F64(b))) => {
            if a.is_nan() && b.is_nan() {
                return;
            }
            assert_eq!(
                a.to_bits(),
                b.to_bits(),
                "float bit mismatch in mode {mode:?}: expected {a} ({:#x}), got {b} ({:#x})",
                a.to_bits(),
                b.to_bits()
            );
        }
        _ => assert_eq!(
            left, right,
            "differential mismatch in mode {mode:?}: expected {left:?}, got {right:?}"
        ),
    }
}

fn assert_differential(source: &str) -> Number {
    let archive = compile_to_bytes(source);

    let (baseline, off_stats, off_enabled) =
        run_with_config(&archive, JitConfig::with_mode(JitMode::Off));
    assert!(!off_enabled);
    assert_eq!(off_stats.compiled_functions, 0);
    assert_eq!(off_stats.code_bytes, 0);

    for mode in [JitMode::Tier1, JitMode::Tier2, JitMode::Auto] {
        let (got, _stats, _enabled) = run_with_config(&archive, JitConfig::with_mode(mode));
        assert_same_number_or_err(&baseline, &got, mode);
    }
    baseline.expect("expected normal numeric return")
}

fn assert_differential_error(source: &str, expected_err_substr: &str) {
    let archive = compile_to_bytes(source);

    let (baseline, _, _) = run_with_config(&archive, JitConfig::with_mode(JitMode::Off));
    let err_msg = baseline
        .as_ref()
        .expect_err("expected runtime error in baseline interpreter");
    assert!(
        err_msg.contains(expected_err_substr),
        "expected {expected_err_substr:?}, got {err_msg:?}"
    );

    for mode in [JitMode::Tier1, JitMode::Tier2, JitMode::Auto] {
        let (got, _, _) = run_with_config(&archive, JitConfig::with_mode(mode));
        assert_same_number_or_err(&baseline, &got, mode);
    }
}

#[test]
fn runtime_jit_introspection_builtins_reflect_mode_and_compilation() {
    let source = r#"
use std.runtime

fn hot_sum(n: i64) -> i64 {
    var i: i64 = 0
    var acc: i64 = 0
    while i < n {
        acc += (i % 8) - 3
        i += 1
    }
    acc
}

fn main() -> i64 {
    let s = hot_sum(20000)
    let en: i64 = if runtime.jit_enabled() { 1 } else { 0 }
    let fns = runtime.jit_compiled_functions().as(i64)
    let bytes = runtime.jit_code_bytes().as(i64)
    let deopts = runtime.jit_deopt_count().as(i64)
    if en == 1 {
        if fns >= 1 and bytes > 0 and deopts == 0 {
            s + 100000
        } else {
            -999
        }
    } else {
        if fns == 0 and bytes == 0 and deopts == 0 {
            s
        } else {
            -888
        }
    }
}
"#;
    let archive = compile_to_bytes(source);

    let (off_res, off_stats, _) = run_with_config(&archive, JitConfig::with_mode(JitMode::Off));
    let base_sum = off_res.unwrap().to_i64_checked().unwrap();
    assert_eq!(base_sum, 10000);
    assert_eq!(off_stats.compiled_functions, 0);

    #[cfg(all(target_arch = "x86_64", any(unix, windows)))]
    for mode in [JitMode::Auto, JitMode::Tier1, JitMode::Tier2] {
        let (res, stats, enabled) = run_with_config(&archive, JitConfig::with_mode(mode));
        assert!(enabled);
        assert_eq!(res.unwrap().to_i64_checked().unwrap(), base_sum + 100000);
        assert!(stats.compiled_functions >= 1);
        assert!(stats.code_bytes > 0);
    }
}

#[test]
fn differential_integer_arithmetic_comparisons_and_nsbc_bitwise_opcodes() {
    let source = r#"
fn kernel(rounds: i64) -> i64 {
    var i: i64 = 1
    var acc: i64 = 17
    while i <= rounds {
        let a = (i * 13) - 7
        let b = (i % 9) + 1
        let q = a / b
        let r = a % b
        let neg_r = -r
        if a < 100 and a <= 100 {
            acc += q + neg_r
        } else if a > 500 and a >= 500 {
            acc -= (r - 1)
        } else if a == b or a != b {
            acc = (acc + q) - r
        }
        i += 1
    }
    acc
}

fn main() -> i64 {
    kernel(80000)
}
"#;
    let res = assert_differential(source);
    assert!(res.to_i64_checked().is_some());

    // Directly exercise NSBC BitAnd, BitOr, BitXor, BitNot, Shl, Shr, Neg opcodes across modes!
    let build_bitwise_program = |mode: JitMode| -> i64 {
        let mut engine = Engine::new(EngineConfig {
            jit: JitConfig::with_mode(mode),
            ..Default::default()
        });
        let instrs = vec![
            Instruction::load_imm(Reg(1), 0),     // pc 0: i = 0
            Instruction::load_imm(Reg(2), 10000), // pc 1: limit = 10000
            Instruction::load_imm(Reg(3), 12345), // pc 2: acc = 12345
            Instruction::load_imm(Reg(4), 1),     // pc 3: step = 1
            Instruction::load_imm(Reg(5), 3),     // pc 4: shift = 3
            Instruction::load_imm(Reg(6), 255),   // pc 5: mask = 255
            // loop header (pc 6):
            Instruction::r_type(Opcode::CmpLt, Reg(7), Reg(1), Reg(2)), // pc 6
            Instruction::jmp_if_not(Reg(7), 12),                        // pc 7 -> pc 19
            Instruction::r_type(Opcode::BitAnd, Reg(8), Reg(1), Reg(6)),
            Instruction::r_type(Opcode::Shl, Reg(9), Reg(8), Reg(5)),
            Instruction::r_type(Opcode::Shr, Reg(10), Reg(9), Reg(4)),
            Instruction::r_type(Opcode::BitXor, Reg(3), Reg(3), Reg(10)),
            Instruction::r_type(Opcode::BitOr, Reg(8), Reg(8), Reg(4)),
            Instruction::r_type(Opcode::BitNot, Reg(9), Reg(8), Reg(0)),
            Instruction::r_type(Opcode::Neg, Reg(9), Reg(9), Reg(0)),
            Instruction::add(Reg(3), Reg(3), Reg(9)),
            Instruction::add(Reg(1), Reg(1), Reg(4)),
            Instruction::safepoint(),
            Instruction::jmp(-12), // pc 18 -> pc 6
            Instruction::mov(Reg(0), Reg(3)),
            Instruction::ret(Reg(0)),
        ];
        engine.vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id: FuncId(0),
            instructions: instrs.into_iter().map(Instruction::encode).collect(),
            register_count: 16,
            param_count: 0,
            is_closure: false,
            function_type: TypeIndex::INVALID,
        });
        let t = engine.vm.spawn_root(FuncId(0));
        assert!(matches!(engine.vm.run(), VmResult::Finished));
        engine.vm.task_result_i64(t).unwrap()
    };

    let expected_bits = build_bitwise_program(JitMode::Off);
    for mode in [JitMode::Tier1, JitMode::Tier2, JitMode::Auto] {
        assert_eq!(build_bitwise_program(mode), expected_bits, "mode={mode:?}");
    }
}

#[test]
fn differential_57bit_to_64bit_to_128bit_integer_overflow_promotion() {
    // Tests exact promotion across the 57-bit IMM_I_SMALL boundary ([-2^56, 2^56 - 1])
    // and across the 64-bit signed boundary into 2-word heap I128.
    let source = r#"
fn promote_add(a: Any, b: Any) -> Any {
    a + b
}

fn promote_sub(a: Any, b: Any) -> Any {
    a - b
}

fn promote_mul(a: Any, b: Any) -> Any {
    a * b
}

fn main() -> i128 {
    -- 2^55 = 36028797018963968 (fits in 57-bit signed IMM_I_SMALL)
    let p55: i64 = 36028797018963968
    let neg_p55: i64 = -36028797018963968
    -- 2^29 = 536870912
    let p29: i64 = 536870912
    -- 2^62 = 4611686018427387904
    let p62: i64 = 4611686018427387904

    -- Crosses +2^56 - 1 -> 1-word boxed I64 (2^56)
    let pos56 = promote_add(p55, p55).as(i128)
    -- Crosses -2^56 -> 1-word boxed I64 (-2^56 - 1)
    let neg56_minus1 = promote_sub(neg_p55, p55 + 1).as(i128)
    -- Crosses 57-bit via imul JO -> 1-word boxed I64 (2^58)
    let mul58 = promote_mul(p29, p29).as(i128)

    -- Crosses 64-bit i64::MAX -> 2-word boxed I128 (2^63)
    let big63 = promote_add(p62, p62).as(i128)

    (big63 - mul58) + pos56 + neg56_minus1
}
"#;
    let res = assert_differential(source);
    let expected: i128 = (1_i128 << 63) - (1_i128 << 58) + (1_i128 << 56) + (-(1_i128 << 56) - 1);
    assert_eq!(res, Number::I128(expected));
}

#[test]
fn differential_hybrid_float_loops_boxed_and_immediate_and_nan_zero_semantics() {
    // 0.1 and 0.3 have non-zero low 7 mantissa bits (heap-boxed F64),
    // while 1.5 and 2.0 have zero low 7 mantissa bits (immediate IMM_F64).
    let source = r#"
fn float_kernel(n: i64) -> f64 {
    var i: i64 = 0
    var acc: f64 = 0.0
    var scale: f64 = 1.5
    while i < n {
        acc = acc + 0.1
        acc = (acc * scale) / 1.5
        if acc > 10.0 {
            acc = acc - 0.3
        }
        i += 1
    }
    acc
}

fn special_floats() -> i64 {
    let neg_zero: f64 = -0.0
    let pos_zero: f64 = 0.0
    let nan_val: f64 = nan()
    var score: i64 = 0
    if neg_zero == pos_zero { score += 1 }
    if not (neg_zero != pos_zero) { score += 2 }
    if not (nan_val == nan_val) { score += 4 }
    if nan_val != nan_val { score += 8 }
    if not (nan_val < 1.0) and not (nan_val >= 1.0) { score += 16 }
    score
}

fn main() -> f64 {
    let v = float_kernel(75000)
    let flags = special_floats()
    if flags == 31 { v } else { -1.0 }
}
"#;
    let res = assert_differential(source);
    assert!(matches!(res, Number::F64(v) if v > 9.0 && v < 11.0));

    // Verify float division by 0.0 inside a Tier-2 loop preserves DivisionByZero error!
    let div_zero_src = r#"
fn bad_div(n: i64) -> f64 {
    var i: i64 = 0
    var acc: f64 = 100.0
    var denom: f64 = 5.0
    while i < n {
        if i == 100 {
            denom = 0.0
        }
        acc = acc / denom
        i += 1
    }
    acc
}

fn main() -> f64 {
    bad_div(200)
}
"#;
    assert_differential_error(div_zero_src, "DivisionByZero");
}

#[test]
fn differential_struct_tuple_enum_and_field_inline_caches() {
    let source = r#"
struct Counter {
    count: i64,
    weight: f64,
    active: bool,
    marker: Unit,
    extra: Any,
}

enum Op {
    Add(delta: i64),
    Scale(factor: i64),
    Reset,
}

newtype Score = i64

fn apply_op(c: Counter, op: Op) -> i64 {
    if not c.active {
        return 0
    }
    -- Exercises LoadField IC on Unit-typed field (`val == UNIT` guard)
    let _u: Unit = c.marker
    op match {
        Op.Add(d) => {
            c.count = c.count + d
            c.weight = c.weight + 0.25
        },
        Op.Scale(f) => {
            c.count = c.count * f
        },
        Op.Reset => {
            c.count = 1
        },
    }
    let pair = (c.count, Score(3))
    pair.0 + pair.1.as(i64)
}

fn main() -> i64 {
    let c = Counter {
        count: 1,
        weight: 0.5,
        active: true,
        marker: (),
        extra: 10,
    }
    var i: i64 = 0
    var total: i64 = 0
    while i < 12000 {
        let op = if i % 97 == 0 {
            Op.Reset
        } else if i % 5 == 0 {
            Op.Scale(2)
        } else {
            Op.Add((i % 8) + 1)
        }
        if c.count > 100000 {
            c.count = c.count / 8
        }
        total += apply_op(c, op)
        i += 1
    }
    total + to_i64(c.weight)
}
"#;
    let res = assert_differential(source);
    assert!(res.to_i64_checked().unwrap() > 0);
}

#[test]
fn differential_closures_recursion_and_checked_errors() {
    let source = r#"
enum LimitErr {
    TooLarge(val: i64),
}

fn checked_step(x: i64, limit: i64) -> !LimitErr i64 {
    if x > limit {
        error LimitErr.TooLarge(x)
    }
    x * 3 + 1
}

fn make_adder(seed: i64) -> fn(i64) -> i64 {
    var hidden = seed
    let f = |delta: i64| {
        hidden += delta
        hidden
    }
    f
}

fn fib(n: i64) -> i64 {
    if n <= 1 { n } else { fib(n - 1) + fib(n - 2) }
}

fn main() -> i64 {
    let next = make_adder(10)
    var i: i64 = 0
    var acc: i64 = 0
    while i < 6000 {
        let v = next(i % 4)
        let stepped = checked_step(v % 150, 120) ! {
            LimitErr.TooLarge(bad) => bad - 100,
        }
        acc += stepped
        i += 1
    }
    acc + fib(18)
}
"#;
    let res = assert_differential(source);
    assert!(res.to_i64_checked().unwrap() > 0);
}

#[test]
fn differential_async_tasks_and_multishot_continuation_clones_across_jit_loops() {
    let source = r#"
use std.task

async effect worker(seed: i64, iters: i64) -> i64
effect choose(catch k: Continuation) -> i64

fn hot_branch_work(base: i64, n: i64) -> i64 {
    var i: i64 = 0
    var acc: i64 = base
    while i < n {
        acc += (i % 16) - 7
        i += 1
    }
    acc
}

fn branchy_pipeline() -> #choose i64 {
    let pre = hot_branch_work(5, 8000)
    let picked = choose()#
    let post = hot_branch_work(picked, 8000)
    pre + post
}

fn run_async_workers() -> #worker i64 {
    let f1 = worker(10, 25000)#
    let f2 = worker(20, 25000)#
    let f3 = worker(30, 25000)#
    f1.await + f2.await + f3.await
}

fn main() -> i64 {
    let saved = branchy_pipeline() # {
        choose(k) => k,
    }
    let k1 = saved.clone()
    let k2 = saved.clone()
    let cont_total = k1(100) + k2(200) + saved(300)

    let async_total = run_async_workers() # {
        async worker(seed, iters) => {
            var i: i64 = 0
            var sum: i64 = seed
            while i < iters {
                sum += (i % 4)
                if i == 12500 {
                    task.yield_now()
                }
                i += 1
            }
            sum
        },
    }

    cont_total + async_total
}
"#;
    let res = assert_differential(source);
    assert!(res.to_i64_checked().unwrap() > 0);
}

#[test]
fn single_call_osr_rescues_five_million_iteration_main_loop() {
    // `main` is called EXACTLY ONCE. Without loop back-edge OSR, a function-entry-only
    // JIT would never compile or enter `main` during this 5,000,000-iteration loop!
    let source = r#"
use std.runtime

fn main() -> i64 {
    var i: i64 = 0
    var int_acc: i64 = 0
    var flt_acc: f64 = 0.0
    while i < 5000000 {
        int_acc = int_acc + (i - (i / 8) * 8) - 3
        -- 0.1 is heap-boxed in the interpreter; Tier-2 OSR unboxes it into XMM
        -- with zero heap allocations inside the loop!
        flt_acc = flt_acc + 0.1
        if flt_acc > 100.0 {
            flt_acc = flt_acc - 50.0
        }
        i += 1
    }
    let compiled = runtime.jit_compiled_functions().as(i64)
    let bytes = runtime.jit_code_bytes().as(i64)
    if runtime.jit_enabled() {
        if compiled < 1 or bytes <= 0 {
            return -1
        }
    }
    int_acc + to_i64(round(flt_acc))
}
"#;
    let archive = compile_to_bytes(source);

    let t_off_start = Instant::now();
    let (off_res, off_stats, _) = run_with_config(&archive, JitConfig::with_mode(JitMode::Off));
    let off_elapsed = t_off_start.elapsed();
    let expected = off_res.expect("Off succeeded").to_i64_checked().unwrap();
    assert_ne!(expected, -1);
    assert_eq!(off_stats.compiled_functions, 0);

    let t_auto_start = Instant::now();
    let (auto_res, auto_stats, auto_enabled) =
        run_with_config(&archive, JitConfig::with_mode(JitMode::Auto));
    let auto_elapsed = t_auto_start.elapsed();
    let auto_val = auto_res
        .expect("Auto OSR succeeded")
        .to_i64_checked()
        .unwrap();
    assert_eq!(auto_val, expected);

    let t_t2_start = Instant::now();
    let (t2_res, t2_stats, _) = run_with_config(&archive, JitConfig::with_mode(JitMode::Tier2));
    let t2_elapsed = t_t2_start.elapsed();
    assert_eq!(t2_res.unwrap().to_i64_checked().unwrap(), expected);

    #[cfg(all(target_arch = "x86_64", any(unix, windows)))]
    {
        assert!(auto_enabled);
        assert!(
            auto_stats.compiled_functions >= 1,
            "expected OSR to compile single-call main, got {auto_stats:?}"
        );
        assert!(auto_stats.code_bytes > 0);
        assert!(t2_stats.compiled_functions >= 1);
        let speedup_auto = off_elapsed.as_secs_f64() / auto_elapsed.as_secs_f64().max(1e-9);
        let speedup_t2 = off_elapsed.as_secs_f64() / t2_elapsed.as_secs_f64().max(1e-9);
        eprintln!(
            "[JIT OSR 5M benchmark] Off={off_elapsed:?}, Auto(OSR)={auto_elapsed:?} ({speedup_auto:.2}x), Tier2={t2_elapsed:?} ({speedup_t2:.2}x), code_bytes={}",
            auto_stats.code_bytes
        );
        assert!(
            auto_elapsed < off_elapsed,
            "expected single-call OSR ({auto_elapsed:?}) to beat interpreter ({off_elapsed:?})"
        );
    }
}

#[test]
fn tier2_mid_loop_57bit_overflow_deoptimizes_and_promotes_cleanly() {
    // Starts in Tier-2 with 57-bit IMM_I_SMALL integers, then crosses 2^56 mid-loop
    // on iteration 70,000 (after Tier-2 OSR entry at 65,536 in Auto mode!),
    // triggering Tier-2 DeoptStub, bumping `jit_deopt_count()`, and promoting in the interpreter!
    let source = r#"
use std.runtime

fn overflow_mid_loop() -> i64 {
    var i: i64 = 0
    var acc: i64 = 0
    -- 2^40 = 1099511627776; 65536 * 2^40 = 2^56 -> overflows 57-bit signed max (2^56 - 1)
    -- right at iteration 65,536 (immediately after Auto Tier-2 OSR entry as well as in Tier2)!
    let big_step: i64 = 1099511627776
    while i < 80000 {
        if i < 70000 {
            acc = acc + big_step
        } else {
            acc = acc + 1
        }
        i += 1
    }
    acc
}

fn main() -> i64 {
    let total = overflow_mid_loop()
    if runtime.jit_enabled() {
        if runtime.jit_deopt_count().as(i64) < 1 {
            return -1
        }
    }
    total
}
"#;
    let archive = compile_to_bytes(source);

    let (off_res, _, _) = run_with_config(&archive, JitConfig::with_mode(JitMode::Off));
    let expected = off_res.unwrap().to_i64_checked().unwrap();
    assert_eq!(expected, 70_000 * (1_i64 << 40) + 10_000);

    for mode in [JitMode::Tier2, JitMode::Auto] {
        let (res, stats, enabled) = run_with_config(&archive, JitConfig::with_mode(mode));
        let got = res.unwrap().to_i64_checked().unwrap();
        assert_eq!(got, expected, "mismatch after deopt in {mode:?}");
        #[cfg(all(target_arch = "x86_64", any(unix, windows)))]
        {
            assert!(enabled);
            assert!(
                stats.deopt_count >= 1,
                "expected Tier-2 overflow deopt in {mode:?}, got {stats:?}"
            );
        }
    }
}

#[test]
fn tiny_code_cache_cap_falls_back_cleanly_to_interpreter() {
    for mode in ALL_MODES {
        let source = r#"
fn main() -> i64 {
    var i: i64 = 0
    var sum: i64 = 0
    while i < 10000 {
        sum += i
        i += 1
    }
    sum
}
"#;
        let archive = compile_to_bytes(source);
        let (res, stats, _) = run_with_config(
            &archive,
            JitConfig {
                mode,
                max_code_bytes: 32,
            },
        );
        assert_eq!(res.unwrap().to_i64_checked().unwrap(), 49_995_000);
        assert_eq!(stats.compiled_functions, 0);
        assert_eq!(stats.code_bytes, 0);
    }
}
