//! End-to-end bytecode & moving-GC tests for native builtins `573..=637`.

use std::path::PathBuf;

use interpreter::{VmError, VmResult};
use nsbc::{Constant, FuncId, FunctionAbi, Instruction, Reg};
use runtime::{BuiltinFnId, FunctionCode, Number, TaskId, ids};
use type_pool::TypeIndex;

use crate::Engine;

#[derive(Clone, Debug)]
enum Arg {
    I64(i64),
    U64(u64),
    I128(i128),
    U128(u128),
    F64(f64),
    Bool(bool),
    Char(char),
    Str(String),
    Null,
    Unit,
    List(Vec<Arg>),
}

impl Arg {
    fn str(s: impl Into<String>) -> Self {
        Self::Str(s.into())
    }
}

#[derive(Clone, Debug)]
enum PathStep {
    TupleElem(i64),
    ListElem(i64),
    ListLen,
    MapGet(String),
    MapLen,
}

struct Harness {
    engine: Engine,
    next_func_id: u32,
}

impl Harness {
    fn new() -> Self {
        Self {
            engine: Engine::with_defaults(),
            next_func_id: 0,
        }
    }

    fn push_const(&mut self, c: Constant) -> u32 {
        self.engine.vm.push_constant(&c).expect("push constant")
    }

    fn emit_arg(
        &mut self,
        arg: &Arg,
        dst_slot: u32,
        next_temp_slot: &mut u32,
        instrs: &mut Vec<Instruction>,
    ) {
        match arg {
            Arg::I64(v) => {
                let idx = self.push_const(Constant::Int(*v));
                instrs.push(Instruction::load_const_wide(Reg(0), idx));
                instrs.push(Instruction::store_slot(dst_slot, Reg(0)));
            }
            Arg::U64(v) => {
                let idx = self.push_const(Constant::UInt(*v));
                instrs.push(Instruction::load_const_wide(Reg(0), idx));
                instrs.push(Instruction::store_slot(dst_slot, Reg(0)));
            }
            Arg::I128(v) => {
                let idx = self.push_const(Constant::Int128(*v));
                instrs.push(Instruction::load_const_wide(Reg(0), idx));
                instrs.push(Instruction::store_slot(dst_slot, Reg(0)));
            }
            Arg::U128(v) => {
                let idx = self.push_const(Constant::UInt128(*v));
                instrs.push(Instruction::load_const_wide(Reg(0), idx));
                instrs.push(Instruction::store_slot(dst_slot, Reg(0)));
            }
            Arg::F64(v) => {
                let idx = self.push_const(Constant::Float(*v));
                instrs.push(Instruction::load_const_wide(Reg(0), idx));
                instrs.push(Instruction::store_slot(dst_slot, Reg(0)));
            }
            Arg::Bool(true) => {
                instrs.push(Instruction::load_true(Reg(0)));
                instrs.push(Instruction::store_slot(dst_slot, Reg(0)));
            }
            Arg::Bool(false) => {
                instrs.push(Instruction::load_false(Reg(0)));
                instrs.push(Instruction::store_slot(dst_slot, Reg(0)));
            }
            Arg::Char(ch) => {
                let idx = self.push_const(Constant::Char(*ch));
                instrs.push(Instruction::load_const_wide(Reg(0), idx));
                instrs.push(Instruction::store_slot(dst_slot, Reg(0)));
            }
            Arg::Str(s) => {
                let idx = self.push_const(Constant::Str(s.clone()));
                instrs.push(Instruction::load_const_wide(Reg(0), idx));
                instrs.push(Instruction::store_slot(dst_slot, Reg(0)));
            }
            Arg::Null => {
                instrs.push(Instruction::load_null(Reg(0)));
                instrs.push(Instruction::store_slot(dst_slot, Reg(0)));
            }
            Arg::Unit => {
                instrs.push(Instruction::load_unit(Reg(0)));
                instrs.push(Instruction::store_slot(dst_slot, Reg(0)));
            }
            Arg::List(items) => {
                let list_slot = *next_temp_slot;
                *next_temp_slot += 1;
                let elem_slot = *next_temp_slot;
                *next_temp_slot += 1;
                instrs.push(Instruction::call_builtin(ids::LIST_INIT, 0));
                instrs.push(Instruction::store_slot(list_slot, Reg(0)));
                for item in items {
                    self.emit_arg(item, elem_slot, next_temp_slot, instrs);
                    instrs.push(Instruction::load_slot(Reg(0), list_slot));
                    instrs.push(Instruction::load_slot(Reg(1), elem_slot));
                    instrs.push(Instruction::call_builtin(ids::LIST_PUSH, 2));
                }
                instrs.push(Instruction::load_slot(Reg(0), list_slot));
                instrs.push(Instruction::store_slot(dst_slot, Reg(0)));
            }
        }
    }

    fn run_call(
        &mut self,
        builtin: BuiltinFnId,
        args: &[Arg],
        path: &[PathStep],
    ) -> Result<TaskId, VmError> {
        let mut instrs = Vec::new();
        let mut next_slot = args.len() as u32 + 1;
        // Reserve a placeholder for AllocateSlots; patched once total slot count is known.
        instrs.push(Instruction::nop());

        for (i, arg) in args.iter().enumerate() {
            self.emit_arg(arg, i as u32, &mut next_slot, &mut instrs);
        }
        for i in 0..args.len() {
            instrs.push(Instruction::load_slot(Reg(i as u8), i as u32));
        }
        instrs.push(Instruction::call_builtin(builtin, args.len() as u8));

        for step in path {
            let cur_slot = next_slot;
            next_slot += 1;
            instrs.push(Instruction::store_slot(cur_slot, Reg(0)));
            match step {
                PathStep::TupleElem(idx) => {
                    let c = self.push_const(Constant::Int(*idx));
                    instrs.push(Instruction::load_slot(Reg(0), cur_slot));
                    instrs.push(Instruction::load_const_wide(Reg(1), c));
                    instrs.push(Instruction::call_builtin(ids::TUPLE_GET, 2));
                }
                PathStep::ListElem(idx) => {
                    let c = self.push_const(Constant::Int(*idx));
                    instrs.push(Instruction::load_slot(Reg(0), cur_slot));
                    instrs.push(Instruction::load_const_wide(Reg(1), c));
                    instrs.push(Instruction::call_builtin(ids::LIST_GET, 2));
                }
                PathStep::ListLen => {
                    instrs.push(Instruction::load_slot(Reg(0), cur_slot));
                    instrs.push(Instruction::call_builtin(ids::LIST_LEN, 1));
                }
                PathStep::MapGet(key) => {
                    let c = self.push_const(Constant::Str(key.clone()));
                    instrs.push(Instruction::load_slot(Reg(0), cur_slot));
                    instrs.push(Instruction::load_const_wide(Reg(1), c));
                    instrs.push(Instruction::call_builtin(ids::MAP_GET, 2));
                }
                PathStep::MapLen => {
                    instrs.push(Instruction::load_slot(Reg(0), cur_slot));
                    instrs.push(Instruction::call_builtin(ids::MAP_LEN, 1));
                }
            }
        }

        instrs.push(Instruction::ret(Reg(0)));
        instrs[0] = Instruction::allocate_slots(next_slot.max(1));

        let func_id = FuncId(self.next_func_id);
        self.next_func_id += 1;
        let reg_count = (args.len() + 2).max(4) as u8;
        self.engine.vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id,
            instructions: instrs.into_iter().map(Instruction::encode).collect(),
            register_count: reg_count,
            param_count: 0,
            is_closure: false,
            function_type: TypeIndex::INVALID,
        });

        let task = self.engine.vm.spawn_root(func_id);
        match self.engine.vm.run() {
            VmResult::Finished => Ok(task),
            VmResult::Error(e) => Err(e),
        }
    }

    fn call_i64(&mut self, builtin: BuiltinFnId, args: &[Arg], path: &[PathStep]) -> i64 {
        let task = self.run_call(builtin, args, path).expect("VM finished");
        self.engine.vm.task_result_i64(task).expect("i64 result")
    }

    fn call_opt_i64(
        &mut self,
        builtin: BuiltinFnId,
        args: &[Arg],
        path: &[PathStep],
    ) -> Option<i64> {
        let task = self.run_call(builtin, args, path).expect("VM finished");
        if self.engine.vm.task_result_is_null(task).unwrap() {
            None
        } else {
            Some(self.engine.vm.task_result_i64(task).expect("i64 result"))
        }
    }

    fn call_u64(&mut self, builtin: BuiltinFnId, args: &[Arg], path: &[PathStep]) -> u64 {
        let task = self.run_call(builtin, args, path).expect("VM finished");
        match self.engine.vm.task_result_number(task).expect("number") {
            Number::U64(v) => v,
            Number::I64(v) => u64::try_from(v).expect("non-negative u64"),
            other => panic!("expected u64, got {other:?}"),
        }
    }

    fn call_f64(&mut self, builtin: BuiltinFnId, args: &[Arg], path: &[PathStep]) -> f64 {
        let task = self.run_call(builtin, args, path).expect("VM finished");
        self.engine
            .vm
            .task_result_number(task)
            .expect("number")
            .to_f64()
    }

    fn call_opt_f64(
        &mut self,
        builtin: BuiltinFnId,
        args: &[Arg],
        path: &[PathStep],
    ) -> Option<f64> {
        let task = self.run_call(builtin, args, path).expect("VM finished");
        if self.engine.vm.task_result_is_null(task).unwrap() {
            None
        } else {
            Some(
                self.engine
                    .vm
                    .task_result_number(task)
                    .expect("number")
                    .to_f64(),
            )
        }
    }

    fn call_bool(&mut self, builtin: BuiltinFnId, args: &[Arg], path: &[PathStep]) -> bool {
        let task = self.run_call(builtin, args, path).expect("VM finished");
        self.engine
            .vm
            .task_result_bool(task)
            .expect("bool")
            .expect("non-null bool")
    }

    fn call_str(&mut self, builtin: BuiltinFnId, args: &[Arg], path: &[PathStep]) -> String {
        let task = self.run_call(builtin, args, path).expect("VM finished");
        self.engine
            .vm
            .task_result_string(task)
            .expect("string")
            .expect("non-null string")
    }

    fn call_opt_str(
        &mut self,
        builtin: BuiltinFnId,
        args: &[Arg],
        path: &[PathStep],
    ) -> Option<String> {
        let task = self.run_call(builtin, args, path).expect("VM finished");
        self.engine.vm.task_result_string(task).expect("opt string")
    }

    fn call_unit(&mut self, builtin: BuiltinFnId, args: &[Arg]) {
        let task = self.run_call(builtin, args, &[]).expect("VM finished");
        assert!(self.engine.vm.task_result_is_unit(task).unwrap());
    }

    fn call_err(&mut self, builtin: BuiltinFnId, args: &[Arg]) -> VmError {
        self.run_call(builtin, args, &[])
            .expect_err("expected VM error")
    }
}

#[test]
fn scalar_hash_coherence_across_scalars_and_rejection_of_collections() {
    let mut h = Harness::new();
    let h_i64 = h.call_u64(ids::SCALAR_HASH, &[Arg::I64(1)], &[]);
    let h_u64 = h.call_u64(ids::SCALAR_HASH, &[Arg::U64(1)], &[]);
    let h_i128 = h.call_u64(ids::SCALAR_HASH, &[Arg::I128(1)], &[]);
    let h_u128 = h.call_u64(ids::SCALAR_HASH, &[Arg::U128(1)], &[]);
    let h_f64 = h.call_u64(ids::SCALAR_HASH, &[Arg::F64(1.0)], &[]);
    assert_eq!(h_i64, h_u64);
    assert_eq!(h_i64, h_i128);
    assert_eq!(h_i64, h_u128);
    assert_eq!(h_i64, h_f64);

    let h_zero = h.call_u64(ids::SCALAR_HASH, &[Arg::I64(0)], &[]);
    assert_eq!(h.call_u64(ids::SCALAR_HASH, &[Arg::F64(-0.0)], &[]), h_zero);
    assert_eq!(h.call_u64(ids::SCALAR_HASH, &[Arg::U128(0)], &[]), h_zero);

    let h_neg5 = h.call_u64(ids::SCALAR_HASH, &[Arg::I64(-5)], &[]);
    assert_eq!(h.call_u64(ids::SCALAR_HASH, &[Arg::I128(-5)], &[]), h_neg5);
    assert_eq!(h.call_u64(ids::SCALAR_HASH, &[Arg::F64(-5.0)], &[]), h_neg5);

    let h_nan1 = h.call_u64(ids::SCALAR_HASH, &[Arg::F64(f64::NAN)], &[]);
    let h_nan2 = h.call_u64(ids::SCALAR_HASH, &[Arg::F64(-f64::NAN)], &[]);
    assert_eq!(h_nan1, h_nan2);

    // Distinct domains for non-numeric scalars.
    let h_true = h.call_u64(ids::SCALAR_HASH, &[Arg::Bool(true)], &[]);
    let h_false = h.call_u64(ids::SCALAR_HASH, &[Arg::Bool(false)], &[]);
    let h_null = h.call_u64(ids::SCALAR_HASH, &[Arg::Null], &[]);
    let h_unit = h.call_u64(ids::SCALAR_HASH, &[Arg::Unit], &[]);
    let h_char = h.call_u64(ids::SCALAR_HASH, &[Arg::Char('A')], &[]);
    let h_str = h.call_u64(ids::SCALAR_HASH, &[Arg::str("A")], &[]);
    assert_ne!(h_true, h_false);
    assert_ne!(h_null, h_unit);
    assert_ne!(h_char, h_str);

    // Non-scalar heap objects reject with TypeError.
    assert!(matches!(
        h.call_err(ids::SCALAR_HASH, &[Arg::List(vec![])]),
        VmError::TypeError
    ));
}

#[test]
fn fmt_sprintf_and_f64_formatting_builtins() {
    let mut h = Harness::new();

    // Numeric, hex-float, binary, char, string, non-string %s, and %q formatting.
    let args = [
        Arg::str("%+06d|%#x|%#b|%.2f|%.1e|%g|%a|%c|%s|%s|%q|%q|%q|%q|%%"),
        Arg::List(vec![
            Arg::I64(42),
            Arg::I64(255),
            Arg::I64(5),
            Arg::F64(std::f64::consts::PI),
            Arg::F64(1250.0),
            Arg::F64(0.0001234),
            Arg::F64(1.5),
            Arg::I64(65),
            Arg::str("ok"),
            Arg::Bool(true),
            Arg::str("a\"b\nc\09"),
            Arg::F64(f64::INFINITY),
            Arg::Bool(false),
            Arg::Null,
        ]),
    ];
    assert_eq!(
        h.call_i64(ids::FMT_SPRINTF, &args, &[PathStep::TupleElem(0)]),
        0
    );
    let formatted = h.call_str(ids::FMT_SPRINTF, &args, &[PathStep::TupleElem(1)]);
    assert_eq!(
        formatted,
        "+00042|0xff|0b101|3.14|1.2e+03|0.0001234|0x1.8p+0|A|ok|true|\"a\\\"b\\nc\\09\"|(1/0)|false|null|%"
    );

    // Error codes: 1 = InvalidSpec, 2 = MissingArg, 3 = TypeMismatch, 4 = OutOfRange.
    assert_eq!(
        h.call_i64(
            ids::FMT_SPRINTF,
            &[Arg::str("%z"), Arg::List(vec![Arg::I64(1)])],
            &[PathStep::TupleElem(0)]
        ),
        1
    );
    assert_eq!(
        h.call_i64(
            ids::FMT_SPRINTF,
            &[Arg::str("%d %d"), Arg::List(vec![Arg::I64(1)])],
            &[PathStep::TupleElem(0)]
        ),
        2
    );
    assert_eq!(
        h.call_i64(
            ids::FMT_SPRINTF,
            &[Arg::str("%d"), Arg::List(vec![Arg::str("nope")])],
            &[PathStep::TupleElem(0)]
        ),
        3
    );
    assert_eq!(
        h.call_i64(
            ids::FMT_SPRINTF,
            &[Arg::str("%c"), Arg::List(vec![Arg::I64(0x110000)])],
            &[PathStep::TupleElem(0)]
        ),
        4
    );

    // 575..=579: F64_TO_FIXED, F64_TO_EXP, F64_TO_PRECISION, F64_FREXP, F64_LDEXP.
    assert_eq!(
        h.call_str(ids::F64_TO_FIXED, &[Arg::F64(12.3456), Arg::I64(2)], &[]),
        "12.35"
    );
    assert!(matches!(
        h.call_err(ids::F64_TO_FIXED, &[Arg::F64(1.0), Arg::I64(101)]),
        VmError::InvalidArgument(_)
    ));

    assert_eq!(
        h.call_str(ids::F64_TO_EXP, &[Arg::F64(1250.0), Arg::I64(2)], &[]),
        "1.25e+3"
    );
    assert_eq!(
        h.call_str(ids::F64_TO_EXP, &[Arg::F64(1250.0), Arg::I64(-1)], &[]),
        "1.25e+3"
    );
    assert!(matches!(
        h.call_err(ids::F64_TO_EXP, &[Arg::F64(1.0), Arg::I64(-2)]),
        VmError::InvalidArgument(_)
    ));

    assert_eq!(
        h.call_str(
            ids::F64_TO_PRECISION,
            &[Arg::F64(123.456), Arg::I64(4)],
            &[]
        ),
        "123.5"
    );
    assert!(matches!(
        h.call_err(ids::F64_TO_PRECISION, &[Arg::F64(1.0), Arg::I64(0)]),
        VmError::InvalidArgument(_)
    ));

    let m = h.call_f64(ids::F64_FREXP, &[Arg::F64(6.0)], &[PathStep::TupleElem(0)]);
    let e = h.call_i64(ids::F64_FREXP, &[Arg::F64(6.0)], &[PathStep::TupleElem(1)]);
    assert_eq!((m, e), (0.75, 3));
    assert_eq!(
        h.call_i64(ids::F64_FREXP, &[Arg::F64(0.0)], &[PathStep::TupleElem(1)]),
        0
    );
    assert_eq!(
        h.call_f64(ids::F64_LDEXP, &[Arg::F64(0.75), Arg::I64(3)], &[]),
        6.0
    );
    assert_eq!(
        h.call_f64(ids::F64_LDEXP, &[Arg::F64(1.0), Arg::I64(-5000)], &[]),
        0.0
    );
}

#[test]
fn lua_patterns_and_string_code_helpers() {
    let mut h = Harness::new();

    // Plain find & negative init.
    assert_eq!(
        h.call_i64(
            ids::PAT_FIND,
            &[
                Arg::str("a%d+b"),
                Arg::str("%d+"),
                Arg::I64(0),
                Arg::Bool(true)
            ],
            &[PathStep::TupleElem(0)]
        ),
        1
    );

    // Captures, empty position capture (), frontier %f, balanced %b(), backreference %1.
    let pat_args = [
        Arg::str("  foo=bar(baz(1))!"),
        Arg::str("%f[%a](%a+)=(%a+)(%b())()"),
        Arg::I64(0),
        Arg::Bool(false),
    ];
    assert_eq!(
        h.call_i64(ids::PAT_FIND, &pat_args, &[PathStep::TupleElem(0)]),
        2
    );
    assert_eq!(
        h.call_i64(ids::PAT_FIND, &pat_args, &[PathStep::TupleElem(1)]),
        17
    );
    assert_eq!(
        h.call_str(
            ids::PAT_FIND,
            &pat_args,
            &[PathStep::TupleElem(2), PathStep::ListElem(0)]
        ),
        "foo"
    );
    assert_eq!(
        h.call_str(
            ids::PAT_FIND,
            &pat_args,
            &[PathStep::TupleElem(2), PathStep::ListElem(2)]
        ),
        "(baz(1))"
    );
    assert_eq!(
        h.call_i64(
            ids::PAT_FIND,
            &pat_args,
            &[PathStep::TupleElem(2), PathStep::ListElem(3)]
        ),
        17
    );

    // Backreference %1.
    assert_eq!(
        h.call_i64(
            ids::PAT_FIND,
            &[
                Arg::str("<<hello>>"),
                Arg::str("([<>])%1.-([<>])%2"),
                Arg::I64(0),
                Arg::Bool(false)
            ],
            &[PathStep::TupleElem(1)]
        ),
        9
    );

    // Anchored `^` at `init` (Lua 5.4 semantics).
    assert_eq!(
        h.call_i64(
            ids::PAT_FIND,
            &[
                Arg::str("xx123"),
                Arg::str("^%d+"),
                Arg::I64(2),
                Arg::Bool(false)
            ],
            &[PathStep::TupleElem(0)]
        ),
        2
    );

    // Malformed pattern returns (-1, -1, [], non-empty error).
    assert_eq!(
        h.call_i64(
            ids::PAT_FIND,
            &[
                Arg::str("abc"),
                Arg::str("(unclosed"),
                Arg::I64(0),
                Arg::Bool(false)
            ],
            &[PathStep::TupleElem(0)]
        ),
        -1
    );
    assert!(
        !h.call_str(
            ids::PAT_FIND,
            &[
                Arg::str("abc"),
                Arg::str("(unclosed"),
                Arg::I64(0),
                Arg::Bool(false)
            ],
            &[PathStep::TupleElem(3)]
        )
        .is_empty()
    );

    // PAT_GSUB: captures, max_n, and 0-length match advancement.
    let gsub_args = [
        Arg::str("hello world"),
        Arg::str("(%w+)"),
        Arg::str("%1-%1"),
        Arg::I64(1),
    ];
    assert_eq!(
        h.call_str(ids::PAT_GSUB, &gsub_args, &[PathStep::TupleElem(0)]),
        "hello-hello world"
    );
    assert_eq!(
        h.call_i64(ids::PAT_GSUB, &gsub_args, &[PathStep::TupleElem(1)]),
        1
    );
    assert_eq!(
        h.call_str(
            ids::PAT_GSUB,
            &[Arg::str("aaa"), Arg::str("a*"), Arg::str("x"), Arg::I64(-1)],
            &[PathStep::TupleElem(0)]
        ),
        "x"
    );
    assert_eq!(
        h.call_i64(
            ids::PAT_GSUB,
            &[Arg::str("aaa"), Arg::str("a*"), Arg::str("x"), Arg::I64(-1)],
            &[PathStep::TupleElem(1)]
        ),
        1
    );

    // 585..=587: STR_PARSE_INT_RADIX, STR_FROM_CODES, STR_UTF8_OFFSET.
    assert_eq!(
        h.call_opt_i64(
            ids::STR_PARSE_INT_RADIX,
            &[Arg::str(" -0x2a "), Arg::I64(0)],
            &[]
        ),
        Some(-42)
    );
    assert_eq!(
        h.call_opt_i64(
            ids::STR_PARSE_INT_RADIX,
            &[Arg::str("1011"), Arg::I64(2)],
            &[]
        ),
        Some(11)
    );
    assert_eq!(
        h.call_opt_i64(
            ids::STR_PARSE_INT_RADIX,
            &[Arg::str("zz"), Arg::I64(10)],
            &[]
        ),
        None
    );
    assert!(matches!(
        h.call_err(ids::STR_PARSE_INT_RADIX, &[Arg::str("10"), Arg::I64(37)]),
        VmError::InvalidArgument(_)
    ));

    assert_eq!(
        h.call_opt_str(
            ids::STR_FROM_CODES,
            &[Arg::List(vec![Arg::I64(78), Arg::I64(0x1F680)])],
            &[]
        )
        .as_deref(),
        Some("N🚀")
    );
    assert_eq!(
        h.call_opt_str(
            ids::STR_FROM_CODES,
            &[Arg::List(vec![Arg::I64(0xD800)])],
            &[]
        ),
        None
    );

    // "aé🚀": 'a' at 0 (1B), 'é' at 1..3 (2B), '🚀' at 3..7 (4B).
    assert_eq!(
        h.call_opt_i64(
            ids::STR_UTF8_OFFSET,
            &[Arg::str("aé🚀"), Arg::I64(0), Arg::I64(5)],
            &[]
        ),
        Some(3)
    );
    assert_eq!(
        h.call_opt_i64(
            ids::STR_UTF8_OFFSET,
            &[Arg::str("aé🚀"), Arg::I64(2), Arg::I64(1)],
            &[]
        ),
        Some(3)
    );
    assert_eq!(
        h.call_opt_i64(
            ids::STR_UTF8_OFFSET,
            &[Arg::str("aé🚀"), Arg::I64(-1), Arg::I64(7)],
            &[]
        ),
        Some(3)
    );
}

#[test]
fn bytes_pack_packsize_and_unpack_roundtrip() {
    let mut h = Harness::new();

    // Fixed-size packsize & variable-size rejection.
    assert_eq!(
        h.call_i64(
            ids::BYTES_PACKSIZE,
            &[Arg::str("<bHi3Jd")],
            &[PathStep::TupleElem(0)]
        ),
        1 + 2 + 3 + 8 + 8
    );
    assert_eq!(
        h.call_i64(
            ids::BYTES_PACKSIZE,
            &[Arg::str("<z")],
            &[PathStep::TupleElem(0)]
        ),
        -1
    );

    // Pack and unpack mixed endian, alignment, fixed/NUL/prefixed strings, ints & floats.
    let fmt = "<bH!4i4z s2 c4 >d";
    let pack_args = [
        Arg::str(fmt),
        Arg::List(vec![
            Arg::I64(-7),
            Arg::I64(0x1234),
            Arg::I64(-123456),
            Arg::str("hi"),
            Arg::str("world"),
            Arg::str("ab"),
            Arg::F64(-42.5),
        ]),
    ];
    assert_eq!(
        h.call_str(ids::BYTES_PACK, &pack_args, &[PathStep::TupleElem(1)]),
        ""
    );
    let len = h.call_i64(
        ids::BYTES_PACK,
        &pack_args,
        &[PathStep::TupleElem(0), PathStep::ListLen],
    );
    let mut raw_bytes = Vec::new();
    for idx in 0..len {
        let b = h.call_i64(
            ids::BYTES_PACK,
            &pack_args,
            &[PathStep::TupleElem(0), PathStep::ListElem(idx)],
        );
        raw_bytes.push(Arg::I64(b));
    }

    let unpack_args = [Arg::str(fmt), Arg::List(raw_bytes), Arg::I64(0)];
    assert_eq!(
        h.call_str(ids::BYTES_UNPACK, &unpack_args, &[PathStep::TupleElem(2)]),
        ""
    );
    assert_eq!(
        h.call_i64(ids::BYTES_UNPACK, &unpack_args, &[PathStep::TupleElem(1)]),
        len
    );
    assert_eq!(
        h.call_i64(
            ids::BYTES_UNPACK,
            &unpack_args,
            &[PathStep::TupleElem(0), PathStep::ListElem(0)]
        ),
        -7
    );
    assert_eq!(
        h.call_i64(
            ids::BYTES_UNPACK,
            &unpack_args,
            &[PathStep::TupleElem(0), PathStep::ListElem(1)]
        ),
        0x1234
    );
    assert_eq!(
        h.call_i64(
            ids::BYTES_UNPACK,
            &unpack_args,
            &[PathStep::TupleElem(0), PathStep::ListElem(2)]
        ),
        -123456
    );
    assert_eq!(
        h.call_str(
            ids::BYTES_UNPACK,
            &unpack_args,
            &[PathStep::TupleElem(0), PathStep::ListElem(3)]
        ),
        "hi"
    );
    assert_eq!(
        h.call_str(
            ids::BYTES_UNPACK,
            &unpack_args,
            &[PathStep::TupleElem(0), PathStep::ListElem(4)]
        ),
        "world"
    );
    assert_eq!(
        h.call_str(
            ids::BYTES_UNPACK,
            &unpack_args,
            &[PathStep::TupleElem(0), PathStep::ListElem(5)]
        ),
        "ab\0\0"
    );
    assert_eq!(
        h.call_f64(
            ids::BYTES_UNPACK,
            &unpack_args,
            &[PathStep::TupleElem(0), PathStep::ListElem(6)]
        ),
        -42.5
    );
}

#[test]
fn file_streams_buffered_seek_and_filesystem_builtins() {
    let mut h = Harness::new();

    // Standard stream invariants (588..=591, 594, 595).
    h.call_unit(ids::IO_EPRINT, &[Arg::str("")]);
    h.call_unit(ids::IO_EPRINTLN, &[Arg::str("nessa-stderr-check")]);
    assert_eq!(
        h.call_i64(ids::IO_FLUSH_STDOUT, &[], &[PathStep::TupleElem(0)]),
        0
    );
    assert_eq!(
        h.call_i64(ids::IO_FLUSH_STDERR, &[], &[PathStep::TupleElem(0)]),
        0
    );
    assert!(h.call_bool(ids::FILE_IS_OPEN, &[Arg::I64(1)], &[]));
    assert_eq!(
        h.call_i64(ids::FILE_CLOSE, &[Arg::I64(1)], &[PathStep::TupleElem(0)]),
        4
    );
    assert_eq!(
        h.call_i64(ids::FILE_CLOSE, &[Arg::I64(999)], &[PathStep::TupleElem(0)]),
        6
    );

    // Anonymous FILE_TEMP with interleaved buffered reads, seeks, and writes!
    let fid = h.call_i64(ids::FILE_TEMP, &[], &[PathStep::TupleElem(1)]);
    assert!(fid >= 3);
    assert!(h.call_bool(ids::FILE_IS_OPEN, &[Arg::I64(fid)], &[]));

    assert_eq!(
        h.call_i64(
            ids::FILE_WRITE_STR,
            &[Arg::I64(fid), Arg::str("first\r\nsecond\n  -12.5 0x10rest")],
            &[PathStep::TupleElem(0)]
        ),
        0
    );
    assert_eq!(
        h.call_i64(ids::FILE_FLUSH, &[Arg::I64(fid)], &[PathStep::TupleElem(0)]),
        0
    );
    // Seek to 0 (Start = 0).
    assert_eq!(
        h.call_i64(
            ids::FILE_SEEK,
            &[Arg::I64(fid), Arg::I64(0), Arg::I64(0)],
            &[PathStep::TupleElem(1)]
        ),
        0
    );
    // Read first line stripping \r\n -> populates internal 8 KiB buffer!
    assert_eq!(
        h.call_opt_str(
            ids::FILE_READ_LINE,
            &[Arg::I64(fid), Arg::Bool(false)],
            &[PathStep::TupleElem(1)]
        )
        .as_deref(),
        Some("first")
    );
    // Logical position via Current (1) + 0 MUST be 7 (not physical OS EOF!).
    assert_eq!(
        h.call_i64(
            ids::FILE_SEEK,
            &[Arg::I64(fid), Arg::I64(1), Arg::I64(0)],
            &[PathStep::TupleElem(1)]
        ),
        7
    );
    // Read second line keeping \n.
    assert_eq!(
        h.call_opt_str(
            ids::FILE_READ_LINE,
            &[Arg::I64(fid), Arg::Bool(true)],
            &[PathStep::TupleElem(1)]
        )
        .as_deref(),
        Some("second\n")
    );
    // Read two numbers (`-12.5` and `0x10` = 16.0) without consuming trailing `rest`!
    assert_eq!(
        h.call_opt_f64(
            ids::FILE_READ_NUMBER,
            &[Arg::I64(fid)],
            &[PathStep::TupleElem(1)]
        ),
        Some(-12.5)
    );
    assert_eq!(
        h.call_opt_f64(
            ids::FILE_READ_NUMBER,
            &[Arg::I64(fid)],
            &[PathStep::TupleElem(1)]
        ),
        Some(16.0)
    );
    // Next read_chars(2) must see "re", and count == 0 before EOF returns Some("")!
    assert_eq!(
        h.call_opt_str(
            ids::FILE_READ_CHARS,
            &[Arg::I64(fid), Arg::I64(0)],
            &[PathStep::TupleElem(1)]
        )
        .as_deref(),
        Some("")
    );
    assert_eq!(
        h.call_opt_str(
            ids::FILE_READ_CHARS,
            &[Arg::I64(fid), Arg::I64(2)],
            &[PathStep::TupleElem(1)]
        )
        .as_deref(),
        Some("re")
    );
    // Overwrite at current buffered position ("st" -> "XY") and read back all!
    assert_eq!(
        h.call_i64(
            ids::FILE_WRITE_BYTES,
            &[
                Arg::I64(fid),
                Arg::List(vec![Arg::I64(b'X' as i64), Arg::I64(b'Y' as i64)])
            ],
            &[PathStep::TupleElem(0)]
        ),
        0
    );
    h.call_i64(
        ids::FILE_SEEK,
        &[Arg::I64(fid), Arg::I64(0), Arg::I64(0)],
        &[PathStep::TupleElem(0)],
    );
    let full = h.call_str(
        ids::FILE_READ_ALL,
        &[Arg::I64(fid)],
        &[PathStep::TupleElem(1)],
    );
    assert!(full.ends_with("0x10reXY"), "got {full:?}");
    // At EOF, read_chars(0) and read_line return null!
    assert_eq!(
        h.call_opt_str(
            ids::FILE_READ_CHARS,
            &[Arg::I64(fid), Arg::I64(0)],
            &[PathStep::TupleElem(1)]
        ),
        None
    );
    assert_eq!(
        h.call_opt_str(
            ids::FILE_READ_LINE,
            &[Arg::I64(fid), Arg::Bool(false)],
            &[PathStep::TupleElem(1)]
        ),
        None
    );

    // Seek back 2 bytes from End (2) and read_bytes.
    h.call_i64(
        ids::FILE_SEEK,
        &[Arg::I64(fid), Arg::I64(2), Arg::I64(-2)],
        &[PathStep::TupleElem(0)],
    );
    assert_eq!(
        h.call_i64(
            ids::FILE_READ_BYTES,
            &[Arg::I64(fid), Arg::I64(10)],
            &[PathStep::TupleElem(1), PathStep::ListElem(0)]
        ),
        b'X' as i64
    );

    assert_eq!(
        h.call_i64(ids::FILE_CLOSE, &[Arg::I64(fid)], &[PathStep::TupleElem(0)]),
        0
    );
    assert!(!h.call_bool(ids::FILE_IS_OPEN, &[Arg::I64(fid)], &[]));
    assert_eq!(
        h.call_i64(ids::FILE_CLOSE, &[Arg::I64(fid)], &[PathStep::TupleElem(0)]),
        6
    );

    // Filesystem operations (605..=615) in an isolated temp directory.
    let tmp_file = h.call_str(
        ids::FS_TEMP_PATH,
        &[Arg::str("nessa_w4_")],
        &[PathStep::TupleElem(1)],
    );
    assert!(h.call_bool(ids::FS_EXISTS, &[Arg::str(&tmp_file)], &[]));
    assert!(h.call_bool(ids::FS_IS_FILE, &[Arg::str(&tmp_file)], &[]));
    assert_eq!(
        h.call_i64(
            ids::FS_REMOVE,
            &[Arg::str(&tmp_file), Arg::Bool(false)],
            &[PathStep::TupleElem(0)]
        ),
        0
    );

    let root_dir: PathBuf = std::env::temp_dir().join(format!(
        "nessa_w4_dir_{}_{}",
        std::process::id(),
        h.call_u64(ids::RNG_NEXT_U64, &[], &[])
    ));
    let root_s = root_dir.to_string_lossy().into_owned();
    let sub_s = root_dir.join("nested").to_string_lossy().into_owned();
    assert_eq!(
        h.call_i64(
            ids::FS_CREATE_DIR,
            &[Arg::str(&sub_s), Arg::Bool(true)],
            &[PathStep::TupleElem(0)]
        ),
        0
    );
    assert!(h.call_bool(ids::FS_IS_DIR, &[Arg::str(&sub_s)], &[]));

    let f1 = root_dir.join("a.txt").to_string_lossy().into_owned();
    let f2 = root_dir.join("b.txt").to_string_lossy().into_owned();
    let f3 = root_dir.join("c.txt").to_string_lossy().into_owned();
    let open_id = h.call_i64(
        ids::FILE_OPEN,
        &[Arg::str(&f1), Arg::str("wx")],
        &[PathStep::TupleElem(1)],
    );
    h.call_i64(
        ids::FILE_WRITE_STR,
        &[Arg::I64(open_id), Arg::str("payload")],
        &[PathStep::TupleElem(0)],
    );
    h.call_i64(
        ids::FILE_CLOSE,
        &[Arg::I64(open_id)],
        &[PathStep::TupleElem(0)],
    );

    // "wx" on existing file returns AlreadyExists (3).
    assert_eq!(
        h.call_i64(
            ids::FILE_OPEN,
            &[Arg::str(&f1), Arg::str("wx")],
            &[PathStep::TupleElem(0)]
        ),
        3
    );

    assert_eq!(
        h.call_i64(
            ids::FS_COPY,
            &[Arg::str(&f1), Arg::str(&f2)],
            &[PathStep::TupleElem(1)]
        ),
        7
    );
    assert_eq!(
        h.call_i64(
            ids::FS_RENAME,
            &[Arg::str(&f2), Arg::str(&f3)],
            &[PathStep::TupleElem(0)]
        ),
        0
    );
    assert_eq!(
        h.call_i64(
            ids::FS_METADATA,
            &[Arg::str(&f3)],
            &[PathStep::TupleElem(1)]
        ),
        7
    );
    assert!(h.call_bool(
        ids::FS_METADATA,
        &[Arg::str(&f3)],
        &[PathStep::TupleElem(2)]
    ));

    // FS_READ_DIR returns sorted entry names: ["a.txt", "c.txt", "nested"].
    assert_eq!(
        h.call_str(
            ids::FS_READ_DIR,
            &[Arg::str(&root_s)],
            &[PathStep::TupleElem(1), PathStep::ListElem(0)]
        ),
        "a.txt"
    );
    assert_eq!(
        h.call_str(
            ids::FS_READ_DIR,
            &[Arg::str(&root_s)],
            &[PathStep::TupleElem(1), PathStep::ListElem(1)]
        ),
        "c.txt"
    );
    assert_eq!(
        h.call_str(
            ids::FS_READ_DIR,
            &[Arg::str(&root_s)],
            &[PathStep::TupleElem(1), PathStep::ListElem(2)]
        ),
        "nested"
    );
    assert_eq!(
        h.call_i64(
            ids::FS_CANONICALIZE,
            &[Arg::str(&f3)],
            &[PathStep::TupleElem(0)]
        ),
        0
    );
    assert_eq!(
        h.call_i64(
            ids::FS_REMOVE,
            &[Arg::str(&root_s), Arg::Bool(true)],
            &[PathStep::TupleElem(0)]
        ),
        0
    );
    assert!(!h.call_bool(ids::FS_EXISTS, &[Arg::str(&root_s)], &[]));
}

#[test]
fn env_time_proc_rng_gc_and_backtrace_builtins() {
    let mut h = Harness::new();

    // 616..=622: ENV_*
    h.engine
        .vm
        .set_script_args(vec!["--flag".to_string(), "input.ns".to_string()]);
    assert_eq!(h.engine.vm.script_args(), &["--flag", "input.ns"]);
    assert_eq!(
        h.call_str(ids::ENV_ARGS, &[], &[PathStep::ListElem(1)]),
        "input.ns"
    );
    assert!(h.call_i64(ids::ENV_VARS, &[], &[PathStep::MapLen]) > 0);
    assert_eq!(
        h.call_opt_str(
            ids::ENV_GET,
            &[Arg::str("NESSA_DEFINITELY_UNSET_VAR_W4")],
            &[]
        ),
        None
    );

    let cwd = h.call_str(ids::ENV_CURRENT_DIR, &[], &[PathStep::TupleElem(1)]);
    assert!(!cwd.is_empty());
    // Set to the same current dir (safe for parallel tests!) and test missing dir error.
    assert_eq!(
        h.call_i64(
            ids::ENV_SET_CURRENT_DIR,
            &[Arg::str(&cwd)],
            &[PathStep::TupleElem(0)]
        ),
        0
    );
    assert_eq!(
        h.call_i64(
            ids::ENV_SET_CURRENT_DIR,
            &[Arg::str("/nonexistent_nessa_w4_dir_xyz")],
            &[PathStep::TupleElem(0)]
        ),
        1
    );
    assert_eq!(h.call_str(ids::ENV_OS, &[], &[]), std::env::consts::OS);
    assert_eq!(h.call_str(ids::ENV_ARCH, &[], &[]), std::env::consts::ARCH);

    // 623..=630: TIME_*
    assert!(h.call_i64(ids::TIME_UNIX_SECS, &[], &[]) > 1_700_000_000);
    assert!(h.call_i64(ids::TIME_UNIX_MILLIS, &[], &[]) > 1_700_000_000_000);
    let t0 = h.call_i64(ids::TIME_MONOTONIC_NANOS, &[], &[]);
    h.call_unit(ids::TIME_SLEEP_MS, &[Arg::I64(0)]);
    let t1 = h.call_i64(ids::TIME_MONOTONIC_NANOS, &[], &[]);
    assert!(t1 >= t0);
    assert!(h.call_f64(ids::TIME_CPU_SECS, &[], &[]) >= 0.0);
    assert!(matches!(
        h.call_err(ids::TIME_SLEEP_MS, &[Arg::I64(-1)]),
        VmError::InvalidArgument(_)
    ));

    // 1970-01-01T00:00:00Z -> Thursday (wday = 5), yday = 1.
    assert_eq!(
        h.call_i64(
            ids::TIME_BREAKDOWN,
            &[Arg::I64(0), Arg::Bool(true)],
            &[PathStep::TupleElem(0)]
        ),
        1970
    );
    assert_eq!(
        h.call_i64(
            ids::TIME_BREAKDOWN,
            &[Arg::I64(0), Arg::Bool(true)],
            &[PathStep::TupleElem(6)]
        ),
        5
    );
    assert_eq!(
        h.call_i64(
            ids::TIME_BREAKDOWN,
            &[Arg::I64(0), Arg::Bool(true)],
            &[PathStep::TupleElem(7)]
        ),
        1
    );
    assert!(!h.call_bool(
        ids::TIME_BREAKDOWN,
        &[Arg::I64(0), Arg::Bool(true)],
        &[PathStep::TupleElem(8)]
    ));

    // TIME_MAKE_UNIX: Jan 32, 1970 00:00:00 UTC normalizes to Feb 1, 1970 = 31 * 86400!
    assert_eq!(
        h.call_opt_i64(
            ids::TIME_MAKE_UNIX,
            &[
                Arg::I64(1970),
                Arg::I64(1),
                Arg::I64(32),
                Arg::I64(0),
                Arg::I64(0),
                Arg::I64(0),
                Arg::I64(-1),
                Arg::Bool(true),
            ],
            &[]
        ),
        Some(31 * 86_400)
    );
    // Pre-epoch Dec 31, 1969 23:59:59 UTC == -1 (must NOT be conflated with mktime failure!).
    assert_eq!(
        h.call_opt_i64(
            ids::TIME_MAKE_UNIX,
            &[
                Arg::I64(1969),
                Arg::I64(12),
                Arg::I64(31),
                Arg::I64(23),
                Arg::I64(59),
                Arg::I64(59),
                Arg::I64(-1),
                Arg::Bool(true),
            ],
            &[]
        ),
        Some(-1)
    );

    assert_eq!(
        h.call_str(
            ids::TIME_STRFTIME,
            &[Arg::str("%F %T %a %%"), Arg::I64(0), Arg::Bool(true)],
            &[PathStep::TupleElem(1)]
        ),
        "1970-01-01 00:00:00 Thu %"
    );
    assert_eq!(
        h.call_i64(
            ids::TIME_STRFTIME,
            &[Arg::str("%Q"), Arg::I64(0), Arg::Bool(true)],
            &[PathStep::TupleElem(0)]
        ),
        1
    );

    // 631..=633: PROC_EXEC, PROC_SHELL, PROC_PID
    assert_eq!(
        h.call_i64(ids::PROC_PID, &[], &[]),
        std::process::id() as i64
    );
    assert_eq!(
        h.call_i64(
            ids::PROC_SHELL,
            &[Arg::str(""), Arg::Null],
            &[PathStep::TupleElem(1)]
        ),
        1
    );
    #[cfg(unix)]
    {
        let exec_args = [
            Arg::str("tr"),
            Arg::List(vec![Arg::str("a-z"), Arg::str("A-Z")]),
            Arg::str("nessa"),
        ];
        assert_eq!(
            h.call_i64(ids::PROC_EXEC, &exec_args, &[PathStep::TupleElem(0)]),
            0
        );
        assert_eq!(
            h.call_str(ids::PROC_EXEC, &exec_args, &[PathStep::TupleElem(2)]),
            "NESSA"
        );
        assert_eq!(
            h.call_str(
                ids::PROC_SHELL,
                &[Arg::str("printf 'hi-%s' 42"), Arg::Null],
                &[PathStep::TupleElem(2)]
            ),
            "hi-42"
        );
    }

    // 634..=636: RNG_SEED, RNG_NEXT_U64, RT_GC_COLLECT
    h.call_i64(
        ids::RNG_SEED,
        &[Arg::I64(2), Arg::I64(12345), Arg::I64(67890)],
        &[PathStep::TupleElem(0)],
    );
    let r1 = h.call_u64(ids::RNG_NEXT_U64, &[], &[]);
    let r2 = h.call_u64(ids::RNG_NEXT_U64, &[], &[]);
    h.call_i64(
        ids::RNG_SEED,
        &[Arg::I64(2), Arg::I64(12345), Arg::I64(67890)],
        &[PathStep::TupleElem(0)],
    );
    assert_eq!(h.call_u64(ids::RNG_NEXT_U64, &[], &[]), r1);
    assert_eq!(h.call_u64(ids::RNG_NEXT_U64, &[], &[]), r2);

    let before_gc = h.call_i64(ids::RT_GC_COLLECT, &[Arg::I64(1)], &[]);
    let after_gc = h.call_i64(ids::RT_GC_COLLECT, &[Arg::I64(0)], &[]);
    assert!(after_gc >= before_gc);

    // 637: RT_BACKTRACE across nested call frames.
    let mut bt_engine = Engine::with_defaults();
    let msg_c = bt_engine
        .vm
        .push_constant(&Constant::Str("boom".to_string()))
        .unwrap();
    // fn#0 calls fn#1; fn#1 calls RT_BACKTRACE("boom", 0).
    bt_engine.vm.add_function(FunctionCode {
        display_owner: None,
        abi: FunctionAbi::value_only(0),
        func_id: FuncId(0),
        instructions: [Instruction::call(1, 0), Instruction::ret(Reg(0))]
            .into_iter()
            .map(Instruction::encode)
            .collect(),
        register_count: 4,
        param_count: 0,
        is_closure: false,
        function_type: TypeIndex::INVALID,
    });
    bt_engine.vm.add_function(FunctionCode {
        display_owner: None,
        abi: FunctionAbi::value_only(0),
        func_id: FuncId(1),
        instructions: [
            Instruction::load_const_wide(Reg(0), msg_c),
            Instruction::load_imm(Reg(1), 0),
            Instruction::call_builtin(ids::RT_BACKTRACE, 2),
            Instruction::ret(Reg(0)),
        ]
        .into_iter()
        .map(Instruction::encode)
        .collect(),
        register_count: 4,
        param_count: 0,
        is_closure: false,
        function_type: TypeIndex::INVALID,
    });
    let bt_task = bt_engine.vm.spawn_root(FuncId(0));
    assert!(matches!(bt_engine.vm.run(), VmResult::Finished));
    let bt = bt_engine.vm.task_result_string(bt_task).unwrap().unwrap();
    assert!(
        bt.starts_with("boom\nstack traceback:\n\t[fn#1]"),
        "got {bt}"
    );
    assert!(bt.contains("in main chunk"), "got {bt}");
}

#[test]
fn multi_allocation_builtins_survive_mmtk_moving_defrag_stress() {
    const MARKER: &str = "NESSA_W4_BUILTIN_MOVING_STRESS_CHILD";
    if std::env::var_os(MARKER).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "builtin_fns::tests::multi_allocation_builtins_survive_mmtk_moving_defrag_stress",
                "--nocapture",
            ])
            .env(MARKER, "1")
            .env("MMTK_STRESS_FACTOR", "1")
            .env("MMTK_PRECISE_STRESS", "true")
            .env("MMTK_IMMIX_ALWAYS_DEFRAG", "true")
            .env("MMTK_IMMIX_DEFRAG_EVERY_BLOCK", "true")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stress child failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return;
    }

    let mut h = Harness::new();
    let start_gc = h.engine.vm.completed_collections();

    // 1. FMT_SPRINTF with multiple heap arguments
    assert_eq!(
        h.call_str(
            ids::FMT_SPRINTF,
            &[
                Arg::str("%s:%d:%q"),
                Arg::List(vec![Arg::str("moving"), Arg::I64(77), Arg::str("gc\ntest")])
            ],
            &[PathStep::TupleElem(1)]
        ),
        "moving:77:\"gc\\ntest\""
    );

    // 2. PAT_FIND returning multiple String and i64 position captures
    assert_eq!(
        h.call_str(
            ids::PAT_FIND,
            &[
                Arg::str("alpha=123,beta=456"),
                Arg::str("(%a+)=(%d+),()(%a+)=(%d+)"),
                Arg::I64(0),
                Arg::Bool(false)
            ],
            &[PathStep::TupleElem(2), PathStep::ListElem(3)]
        ),
        "beta"
    );

    // 3. BYTES_PACK + BYTES_UNPACK with multiple strings and numbers
    let packed_fmt = "<i4 z s2";
    let pack_args = [
        Arg::str(packed_fmt),
        Arg::List(vec![
            Arg::I64(987654),
            Arg::str("zero_term_payload"),
            Arg::str("sized_payload"),
        ]),
    ];
    let nbytes = h.call_i64(
        ids::BYTES_PACK,
        &pack_args,
        &[PathStep::TupleElem(0), PathStep::ListLen],
    );
    let mut byte_args = Vec::new();
    for i in 0..nbytes {
        byte_args.push(Arg::I64(h.call_i64(
            ids::BYTES_PACK,
            &pack_args,
            &[PathStep::TupleElem(0), PathStep::ListElem(i)],
        )));
    }
    assert_eq!(
        h.call_str(
            ids::BYTES_UNPACK,
            &[Arg::str(packed_fmt), Arg::List(byte_args), Arg::I64(0)],
            &[PathStep::TupleElem(0), PathStep::ListElem(2)]
        ),
        "sized_payload"
    );

    // 4. FS_READ_DIR & ENV_VARS & TIME_BREAKDOWN (9-element tuple)
    let tmp_dir = std::env::temp_dir().join(format!("nessa_stress_dir_{}", std::process::id()));
    let tmp_dir_s = tmp_dir.to_string_lossy().into_owned();
    h.call_i64(
        ids::FS_CREATE_DIR,
        &[Arg::str(&tmp_dir_s), Arg::Bool(true)],
        &[PathStep::TupleElem(0)],
    );
    for name in ["e1.txt", "e2.txt", "e3.txt"] {
        let p = tmp_dir.join(name).to_string_lossy().into_owned();
        let fid = h.call_i64(
            ids::FILE_OPEN,
            &[Arg::str(&p), Arg::str("w")],
            &[PathStep::TupleElem(1)],
        );
        h.call_i64(ids::FILE_CLOSE, &[Arg::I64(fid)], &[PathStep::TupleElem(0)]);
    }
    assert_eq!(
        h.call_str(
            ids::FS_READ_DIR,
            &[Arg::str(&tmp_dir_s)],
            &[PathStep::TupleElem(1), PathStep::ListElem(2)]
        ),
        "e3.txt"
    );
    h.call_i64(
        ids::FS_REMOVE,
        &[Arg::str(&tmp_dir_s), Arg::Bool(true)],
        &[PathStep::TupleElem(0)],
    );

    assert_eq!(
        h.call_opt_str(ids::ENV_VARS, &[], &[PathStep::MapGet(MARKER.to_string())])
            .as_deref(),
        Some("1")
    );
    assert_eq!(
        h.call_i64(
            ids::TIME_BREAKDOWN,
            &[Arg::I64(86_400), Arg::Bool(true)],
            &[PathStep::TupleElem(2)]
        ),
        2
    );

    assert!(
        h.engine.vm.completed_collections() > start_gc + 20,
        "expected aggressive moving GC collections during stress test"
    );
}
