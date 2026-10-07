//! Effect signatures preserve runtime-provided continuation bindings separately
//! from arguments supplied by effect callers.

mod common;

use ast::NodeKind;
use common::run_value;
use diagnostic::{DiagnosticContext, Level};
use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};
use type_pool::{Intrinsic, TypeKind};

fn resolve(source: &str) -> resolution::ResolvedAst {
    let (tokens, errors) = lexer::tokenize(source);
    assert!(errors.is_empty());
    let source_map = SourceMap::new(FilePathMapping::empty());
    let file = source_map.new_source_file(FileName::Custom("effect-test".into()), source.into());
    let diagnostics = DiagnosticContext::new(&source_map);
    let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
    let resolved = resolution::resolve(ast, &diagnostics);
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
    assert!(
        !resolved
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.level == Level::Error)
    );
    resolved
}

#[test]
fn catch_binding_has_its_own_ast_kind_and_is_excluded_from_effect_arguments() {
    let resolved =
        resolve("effect choose(value: i32, catch k: Continuation, flag: bool) -> String");
    let definition = resolved.ast.multi_children(resolved.ast.root)[0];
    let parameters = resolved.ast.multi_children(definition);
    assert_eq!(parameters.len(), 3);
    assert_eq!(resolved.ast.node(parameters[1]).kind, NodeKind::ParamCatch);
    let binding = resolved.ast.fixed_children(parameters[1])[0];
    assert_eq!(str_interner::get(resolved.ast.node(binding).str_id), "k");
    let symbol = resolved.node_symbols[&binding];
    assert_eq!(
        resolved.symbols[symbol.0 as usize].type_index,
        Intrinsic::Continuation.type_index()
    );
    let effect = &resolved.effects[0];
    let operation = &effect.operations[0];
    assert_eq!(operation.continuation_param, Some(1));
    assert_eq!(
        operation.param_types,
        vec![Intrinsic::I32.type_index(), Intrinsic::Bool.type_index()]
    );
    assert_eq!(operation.return_type, Intrinsic::Str.type_index());
    assert!(matches!(&resolved.type_pool.get(effect.type_index).kind,
        TypeKind::Effect { params, ret, is_async: false }
        if params == &operation.param_types && *ret == operation.return_type));
}

#[test]
fn implicit_catch_type_is_continuation_and_omitted_return_type_is_unit() {
    let resolved = resolve("effect suspend(catch k)");
    let operation = &resolved.effects[0].operations[0];
    assert!(operation.param_types.is_empty());
    assert_eq!(operation.continuation_param, Some(0));
    assert_eq!(operation.return_type, Intrinsic::Unit.type_index());
}

#[test]
fn ordinary_and_async_effect_signatures_resolve_parameter_and_return_types() {
    let resolved =
        resolve("effect read(path: String) -> i64\nasync effect send(data: String) -> bool");
    let read = resolved
        .effects
        .iter()
        .find(|effect| str_interner::get(effect.name) == "read")
        .unwrap();
    assert!(!read.is_async);
    assert_eq!(
        read.operations[0].param_types,
        vec![Intrinsic::Str.type_index()]
    );
    assert_eq!(read.operations[0].return_type, Intrinsic::I64.type_index());
    assert_eq!(read.operations[0].continuation_param, None);
    let send = resolved
        .effects
        .iter()
        .find(|effect| str_interner::get(effect.name) == "send")
        .unwrap();
    assert!(send.is_async);
    assert_eq!(
        send.operations[0].param_types,
        vec![Intrinsic::Str.type_index()]
    );
    assert_eq!(send.operations[0].return_type, Intrinsic::Bool.type_index());
}

#[test]
fn invalid_catch_declarations_report_errors_without_emitting_bytecode() {
    for (source, expected) in [
        (
            "effect yield(catch k, catch other)",
            "only one continuation",
        ),
        (
            "effect yield(catch k: i32)",
            "must have type `Continuation`",
        ),
        (
            "effect yield(catch : Continuation)",
            "continuation identifier",
        ),
        ("effect yield(catch k:)", "continuation type"),
        ("fn main(catch k: Continuation) {}", ""),
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "accepted {source}");
        assert!(result.codegen_output.functions.is_empty());
        assert!(
            result.diagnostics.iter().any(|diagnostic| {
                diagnostic.level == Level::Error && diagnostic.message.contains(expected)
            }),
            "{source}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn source_effect_handler_receives_arguments_and_resumes_computation() {
    assert_eq!(
        run_value(
            "effect ask(value: i64) -> i64\nfn compute() -> #ask i64 { ask(40)# + 1 }\nfn main() { compute()# { ask(value) => value + 1 } }"
        ),
        Ok(42)
    );
}

#[test]
fn source_continuation_can_resume_twice_and_add_branch_results() {
    assert_eq!(
        run_value(
            "effect choose(catch k: Continuation) -> i64\nfn compute() { let value = choose()#\nvalue * 10 }\nfn main() { compute()# { choose(k) => k(1) + k(2) } }"
        ),
        Ok(30)
    );
}

#[test]
fn captured_handler_can_abandon_the_remaining_computation() {
    assert_eq!(
        run_value(
            "effect stop(catch k) -> i64\nfn compute() { stop()# + 100 }\nfn main() { compute()# { stop(k) => 7 } }"
        ),
        Ok(7)
    );
}

#[test]
fn capture_binding_can_appear_between_caller_arguments() {
    assert_eq!(
        run_value(
            "effect choose(a: i64, catch k, b: i64) -> i64\nfn compute() { choose(10, 20)# + 12 }\nfn main() { compute()# { choose(a, k, b) => k(a + b) } }"
        ),
        Ok(42)
    );
}

#[test]
fn repeated_capture_preserves_deep_handlers_in_each_branch() {
    assert_eq!(
        run_value(
            "effect choose(catch k) -> i64\nfn compute() { let a = choose()#\na * 10 + choose()# }\nfn main() { compute()# { choose(k) => k(1) + k(2) } }"
        ),
        Ok(66)
    );
}

#[test]
fn multiple_capturing_effects_share_their_elimination_boundary() {
    assert_eq!(
        run_value(
            "effect left(catch k) -> i64\neffect right(catch k) -> i64\nfn compute() { let a = left()#\na + right()# }\nfn main() { compute()# { left(k) => k(10), right(k) => k(32) } }"
        ),
        Ok(42)
    );
}

#[test]
fn guarded_resume_only_returns_when_its_condition_is_true() {
    assert_eq!(
        run_value(
            "effect ask(value: i64) -> i64\nfn compute() { let a = ask(0)#\na + ask(1)# }\nfn main() { compute()# { ask(value) => { resume 10 if value == 0\n32 } } }"
        ),
        Ok(42)
    );
}

#[test]
fn escaped_continuation_can_resume_after_original_handler_is_removed() {
    assert_eq!(
        run_value(
            "effect suspend(catch k) -> i64\nfn compute() { suspend()# + 2 }\nfn main() { let saved = compute()# { suspend(k) => k }\nsaved(40) }"
        ),
        Ok(42)
    );
}

#[test]
fn delayed_resume_keeps_handlers_for_later_effects() {
    assert_eq!(
        run_value(
            "effect suspend(catch k) -> i64\neffect ask() -> i64\nfn compute() { let value = suspend()#\nvalue + ask()# }\nfn main() { let offset = 2\nlet saved = compute()# { suspend(k) => k, ask() => offset }\nsaved(40) }"
        ),
        Ok(42)
    );
}

#[test]
fn delayed_resume_can_capture_a_second_continuation() {
    assert_eq!(
        run_value(
            "effect suspend(catch k) -> i64\nfn compute() { let a = suspend()#\na + suspend()# }\nfn main() { let first = compute()# { suspend(k) => k }\nlet second = first(10)\nsecond(32) }"
        ),
        Ok(42)
    );
}

#[test]
fn source_clone_creates_a_separately_callable_continuation() {
    assert_eq!(
        run_value(
            "effect suspend(catch k) -> i64\nfn compute() { suspend()# + 10 }\nfn main() { let saved = compute()# { suspend(k) => k }\nlet copy = saved.clone()\nsaved(1) + copy(2) }"
        ),
        Ok(23)
    );
}

#[test]
fn false_resume_guard_does_not_evaluate_its_effectful_value() {
    assert_eq!(
        run_value(
            "effect ask() -> i64\neffect missing() -> i64\nfn main() { ask()# { ask() => { resume missing()# if false\n42 } } }"
        ),
        Ok(42)
    );
}

#[test]
fn unconditional_resume_is_not_overwritten_by_following_statements() {
    assert_eq!(
        run_value("effect ask() -> i64\nfn main() { ask()# { ask() => { resume 42\n99 } } }"),
        Ok(42)
    );
}

#[test]
fn invalid_continuation_calls_and_resume_guards_are_rejected() {
    for (source, message) in [
        (
            "effect suspend(catch k) -> i64\nfn main() { suspend()# { suspend(k) => k() } }",
            "requires exactly one value",
        ),
        (
            "effect suspend(catch k) -> i64\nfn main() { suspend()# { suspend(k) => k(1, 2) } }",
            "requires exactly one value",
        ),
        (
            "effect suspend(catch k) -> i64\nfn main() { suspend()# { suspend(k) => k.clone(1) } }",
            "expects 0 arguments",
        ),
        (
            "effect ask() -> i64\nfn main() { ask()# { ask() => { resume 1 if 2\n42 } } }",
            "guard must have type `bool`",
        ),
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "accepted {source}");
        assert!(result.codegen_output.functions.is_empty());
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(message)),
            "{:?}",
            result.diagnostics
        );
    }
}

#[test]
fn fresh_single_use_handler_binding_resumes_without_branch_copying() {
    let source = "effect choose(catch k) -> i64\nfn compute() { choose()# + 2 }\nfn main() { compute()# { choose(k) => k(40) } }";
    let result = driver::Driver::new().compile(source);
    assert!(!result.has_errors, "{:?}", result.diagnostics);
    let opcodes: Vec<_> = result
        .codegen_output
        .functions
        .iter()
        .flat_map(|function| &function.instructions)
        .map(|&word| nsbc::Instruction::decode(word).unwrap().opcode)
        .collect();
    assert_eq!(
        opcodes
            .iter()
            .filter(|&&opcode| opcode == nsbc::Opcode::ResumeContinuationOnce)
            .count(),
        1
    );
    assert!(!opcodes.contains(&nsbc::Opcode::ResumeContinuation));
    assert_eq!(run_value(source), Ok(42));
}

#[test]
fn escaped_aliased_and_repeated_continuations_keep_multishot_calls() {
    for source in [
        "effect choose(catch k) -> i64\nfn main() { choose()# { choose(k) => k(1) + k(2) } }",
        "effect choose(catch k) -> i64\nfn main() { choose()# { choose(k) => { let alias = k\nk(40) } } }",
        "effect choose(catch k) -> i64\nfn main() { let saved = choose()# { choose(k) => k }\nsaved(40) }",
        "fn invoke(k: Continuation) { k(40) }\nfn main() { 42 }",
        "effect relay(k: Continuation) -> i64\nfn route(k: Continuation) { relay(k)# { relay(value) => value(40) } }\nfn main() { 42 }",
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(!result.has_errors, "{:?}", result.diagnostics);
        assert!(
            result
                .codegen_output
                .functions
                .iter()
                .flat_map(|function| &function.instructions)
                .any(|&word| nsbc::Instruction::decode(word).unwrap().opcode
                    == nsbc::Opcode::ResumeContinuation)
        );
        assert!(
            !result
                .codegen_output
                .functions
                .iter()
                .flat_map(|function| &function.instructions)
                .any(|&word| nsbc::Instruction::decode(word).unwrap().opcode
                    == nsbc::Opcode::ResumeContinuationOnce)
        );
    }
}

#[test]
fn ordinary_continuation_effect_argument_does_not_transfer_ownership() {
    assert_eq!(
        run_value(
            "effect suspend(catch k) -> i64\neffect relay(value: Continuation) -> i64\nfn compute() { suspend()# + 10 }\nfn main() { let saved = compute()# { suspend(k) => k }\nlet first = relay(saved)# { relay(value) => value(1) }\nfirst + saved(2) }"
        ),
        Ok(23)
    );
}

#[test]
fn effect_encoding_limits_report_diagnostics_before_codegen() {
    let parameters = (0..32)
        .map(|index| format!("arg{index}: i64"))
        .collect::<Vec<_>>()
        .join(", ");
    let declarations = (0..32)
        .map(|index| format!("effect item{index}(catch k) -> i64\n"))
        .collect::<String>();
    let handlers = (0..32)
        .map(|index| format!("item{index}(k) => k(1)"))
        .collect::<Vec<_>>()
        .join(", ");
    for (source, message) in [
        (
            format!("effect wide({parameters}) -> i64"),
            "at most 31 arguments",
        ),
        (
            format!("{declarations}fn main() {{ item0()# {{ {handlers} }} }}"),
            "at most 31 handlers",
        ),
    ] {
        let result = driver::Driver::new().compile(&source);
        assert!(result.has_errors);
        assert!(result.codegen_output.functions.is_empty());
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(message)),
            "{:?}",
            result.diagnostics
        );
    }
}

#[test]
fn local_slots_survive_capture_and_multiple_resumption_branches() {
    let declarations = (0..40)
        .map(|index| format!("let x{index} = {index}\n"))
        .collect::<String>();
    let source = format!(
        "effect choose(catch k) -> i64\nfn compute() {{ {declarations} let choice = choose()#\nx0 + x39 + choice }}\nfn main() {{ compute()# {{ choose(k) => k(1) + k(2) }} }}"
    );
    assert_eq!(run_value(&source), Ok(81));
}

#[test]
fn handler_closure_captures_surrounding_values() {
    assert_eq!(
        run_value(
            "effect ask(value: i64) -> i64\nfn main() { let offset = 2\nask(40)# { ask(value) => value + offset } }"
        ),
        Ok(42)
    );
}

#[test]
fn nested_handler_for_same_effect_is_uninstalled_on_exit() {
    assert_eq!(
        run_value(
            "effect ask() -> i64\nfn nested() { ask()# { ask() => 1 } }\nfn compute() { let inner = nested()\ninner + ask()# }\nfn main() { compute()# { ask() => 41 } }"
        ),
        Ok(42)
    );
}

#[test]
fn multiple_effects_dispatch_to_their_corresponding_arms() {
    assert_eq!(
        run_value(
            "effect left() -> i64\neffect right() -> i64\nfn compute() { let a = left()#\na + right()# }\nfn main() { compute()# { left() => 10, right() => 32 } }"
        ),
        Ok(42)
    );
}

#[test]
fn unhandled_effect_returns_a_runtime_error() {
    let error = run_value("effect ask() -> i64\nfn main() { ask()# }").unwrap_err();
    assert!(error.contains("UnhandledEffect"), "{error}");
}

#[test]
fn invalid_handler_targets_bindings_and_effect_arguments_are_rejected() {
    for (source, message) in [
        (
            "effect ask() -> i64\nfn main() { ask()# { ask(value) => value } }",
            "handler expects 0 parameters",
        ),
        (
            "effect ask() -> i64\nfn main() { ask()# { ask() => 1, ask() => 2 } }",
            "duplicate handler",
        ),
        (
            "fn normal() { 1 }\nfn main() { normal()# { normal() => 2 } }",
            "target must be an effect",
        ),
        (
            "effect ask(value: i64) -> i64\nfn main() { ask(true)# { ask(value) => value } }",
            "type mismatch in argument",
        ),
        (
            "effect ask() -> i64\nfn main() { ask()# { ask() => true } }",
            "handler result type mismatch",
        ),
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "accepted {source}");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(message)),
            "{:?}",
            result.diagnostics
        );
    }
}
