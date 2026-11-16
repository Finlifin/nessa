//! Actionable semantic suggestions, typo recovery (`did_you_mean`), unimported
//! standard-library hints, and cross-language idiom diagnostics.

use std::collections::HashSet;

use ast::{Ast, NodeIndex, NodeKind};
use diagnostic::{
    Level, codes,
    suggest::{did_you_mean, suggest_similar},
};
use str_interner::StrId;
use type_pool::{CollectionRole, Intrinsic, TypeIndex, TypeKind};

use crate::resolver::Resolver;
use crate::typing::type_display_name;
use crate::{ScopeId, SymbolId, SymbolKind};

/// Emit `[3001] undefined name` with scope-aware Damerau-Levenshtein suggestions,
/// unimported `std.*` module hints, and foreign-identifier explanations.
pub(crate) fn emit_undefined_name(r: &Resolver<'_>, ast: &Ast, node_idx: NodeIndex, str_id: StrId) {
    let name = str_interner::get(str_id);
    let span = ast.node(node_idx).span;

    let mut builder = r
        .diag_ctx
        .error(format!("undefined name `{name}`"))
        .with_code(codes::UNDEFINED_NAME)
        .with_primary_span(span)
        .with_label(
            span,
            format!("`{name}` is not defined in this scope"),
            Level::Error,
        );

    // 1. Foreign-language literal / builtin muscle-memory hints.
    if let Some(foreign_help) = foreign_identifier_help(&name) {
        builder = builder.with_help(foreign_help.to_string());
    }

    // 2. Unimported standard library / official package symbol hints.
    if let Some(import_hint) = stdlib_import_hint(&name) {
        builder = builder.with_help(import_hint);
    }

    // 3. Lexical scope chain Damerau-Levenshtein typo suggestions.
    let candidates = visible_scope_names(r, r.current_scope);
    if let Some(help) = did_you_mean(&name, candidates.iter().map(String::as_str)) {
        builder = builder.with_help(help);
    }

    builder.emit(r.diag_ctx);
}

fn foreign_identifier_help(name: &str) -> Option<&'static str> {
    match name {
        "None" | "nil" | "undefined" | "nullptr" => {
            Some("Nessa uses `null` for empty optional values (`?T`)")
        }
        "Some" => {
            Some("Nessa wraps optional values implicitly (`let x: ?i64 = 42`), without `Some(...)`")
        }
        "True" => Some("boolean literals in Nessa are lowercase: write `true`"),
        "False" => Some("boolean literals in Nessa are lowercase: write `false`"),
        "console" | "fmt" | "System" => {
            Some("use `println(...)` or `print(...)` for standard output in Nessa")
        }
        "len" => Some("collection and string lengths use method syntax: `xs.len()`"),
        "str" => Some("the string type in Nessa is `String` (convert values via `x.to_string()`)"),
        "int" | "Int" => {
            Some("integer types in Nessa use explicit widths such as `i64`, `i32`, or `usize`")
        }
        "float" | "Float" | "double" => Some("floating-point types in Nessa are `f64` and `f32`"),
        "boolean" | "Bool" => Some("the boolean type in Nessa is lowercase `bool`"),
        "void" => Some("Nessa uses `Unit` (or omits `-> Unit`) instead of `void`"),
        _ => None,
    }
}

fn stdlib_import_hint(name: &str) -> Option<String> {
    if matches!(
        name,
        "bytes"
            | "collections"
            | "coroutine"
            | "env"
            | "fmt"
            | "fs"
            | "io"
            | "json"
            | "math"
            | "net"
            | "path"
            | "process"
            | "rand"
            | "sync"
            | "task"
            | "time"
    ) {
        return Some(format!(
            "consider importing `std.{name}` with `use std.{name}`"
        ));
    }
    let module_path = match name {
        "Channel" | "Mutex" | "RwLock" | "Semaphore" | "Barrier" | "Once" | "Lazy"
        | "AtomicInt" | "AtomicBool" | "WaitGroup" => "std.sync",
        "TcpListener" | "TcpStream" | "UdpSocket" => "std.net",
        "File" | "eprint" | "eprintln" => "std.io",
        "Coroutine" | "Generator" => "std.coroutine",
        "await_all" | "await_any" | "await_first" | "with_timeout" => "std.task",
        "Instant" | "Duration" | "sleep_ms" => "std.time",
        "Deque" | "Set" | "HashMap" => "std.collections",
        "Regex" => "regex",
        _ => return None,
    };
    Some(format!(
        "consider importing `{module_path}.{name}` with `use {module_path}.{name}`"
    ))
}

fn visible_scope_names(r: &Resolver<'_>, start: ScopeId) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut current = Some(start);
    while let Some(scope_id) = current {
        let Some(scope) = r.scopes.get(scope_id.0 as usize) else {
            break;
        };
        for (&name_id, &sym_id) in &scope.bindings {
            let sym = &r.symbols[sym_id.0 as usize];
            if sym.visibility == crate::Visibility::Private && scope_id != start {
                continue;
            }
            let text = str_interner::get(name_id);
            if !text.starts_with("__") && seen.insert(text.clone()) {
                out.push(text);
            }
        }
        current = scope.parent;
    }
    out
}

/// Collect candidate member/field/method names declared on `ty`.
pub(crate) fn type_member_names(r: &Resolver<'_>, ty: TypeIndex) -> Vec<String> {
    let Some(canon) = r.type_pool.canonical_type(ty) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    match &r.type_pool.get(canon).kind {
        TypeKind::Struct { fields, .. } => {
            for f in fields {
                let s = str_interner::get(f.name);
                if seen.insert(s.clone()) {
                    out.push(s);
                }
            }
        }
        TypeKind::Enum { variants, .. } => {
            for v in variants {
                let s = str_interner::get(v.name);
                if seen.insert(s.clone()) {
                    out.push(s);
                }
            }
        }
        _ => {}
    }

    for scope in &r.scopes {
        if scope.assoc_type == Some(canon) {
            for &name_id in scope.bindings.keys() {
                let s = str_interner::get(name_id);
                if !s.starts_with("__") && seen.insert(s.clone()) {
                    out.push(s);
                }
            }
        }
    }

    out
}

/// Build actionable `help:` lines when `member_name` is not found on `receiver_ty`.
pub(crate) fn unknown_member_helps(
    r: &Resolver<'_>,
    receiver_ty: TypeIndex,
    member_name: &str,
) -> Vec<String> {
    let mut helps = Vec::new();
    let Some(canon) = r.type_pool.canonical_type(receiver_ty) else {
        return helps;
    };

    // 1. Check wrapper types (`Future(T)`, `?T`, `!E T`) whose inner payload DOES have `member_name`!
    match &r.type_pool.get(canon).kind {
        TypeKind::Future { inner } => {
            let inner = *inner;
            let inner_members = type_member_names(r, inner);
            if r.type_pool.as_intrinsic(inner) == Some(Intrinsic::Any)
                || inner_members.iter().any(|m| m == member_name)
            {
                helps.push(format!(
                    "this receiver is a `{}`; await it first: `<expr>.await.{member_name}`",
                    type_display_name(&r.type_pool, canon)
                ));
                return helps;
            }
        }
        TypeKind::Optional { inner } => {
            let inner = *inner;
            let inner_members = type_member_names(r, inner);
            if inner_members.iter().any(|m| m == member_name) {
                helps.push(format!(
                    "this receiver is optional (`{}`); propagate `null` first with `<expr>?.{member_name}` or check `<expr> != null`",
                    type_display_name(&r.type_pool, canon)
                ));
                return helps;
            }
        }
        TypeKind::ErrorQualified { inner, .. } => {
            let inner = *inner;
            let inner_members = type_member_names(r, inner);
            if inner_members.iter().any(|m| m == member_name) {
                helps.push(format!(
                    "this receiver is an error-qualified result (`{}`); propagate errors first with `<expr>!.{member_name}`",
                    type_display_name(&r.type_pool, canon)
                ));
                return helps;
            }
        }
        _ => {}
    }

    // 2. Cross-language method / property aliases.
    let role = r.type_pool.collection_role(canon);
    let is_str = r.type_pool.as_intrinsic(canon) == Some(Intrinsic::Str);
    match member_name {
        "length" | "size" => {
            helps.push("use `.len()` to get the number of elements or bytes".into());
        }
        "substr" | "substring" if is_str => {
            helps.push("use `.slice(start, end)` to extract a substring".into());
        }
        "toUpperCase" | "upper" if is_str => {
            helps.push("use `.to_upper()` to convert a String to uppercase".into());
        }
        "toLowerCase" | "lower" if is_str => {
            helps.push("use `.to_lower()` to convert a String to lowercase".into());
        }
        "startsWith" if is_str => {
            helps.push("use `.starts_with(prefix)`".into());
        }
        "endsWith" if is_str => {
            helps.push("use `.ends_with(suffix)`".into());
        }
        "indexOf" if is_str || role == Some(CollectionRole::List) => {
            helps.push("use `.find(...)` or `.index_of(...)`".into());
        }
        "contains_key" | "has_key" | "has" if role == Some(CollectionRole::Map) => {
            helps.push("use `.contains(key)` on `Map`".into());
        }
        "add" if role == Some(CollectionRole::List) => {
            helps.push("use `.push(value)` to append an element to a `List`".into());
        }
        _ => {}
    }

    // 3. Typo suggestion against known members on `receiver_ty`.
    let candidates = type_member_names(r, canon);
    if let Some(dym) = did_you_mean(member_name, candidates.iter().map(String::as_str)) {
        helps.push(dym);
    }

    helps
}

/// Suggest similar field names for struct construction / destructuring.
pub(crate) fn unknown_struct_field_help(
    ast: &Ast,
    field_nodes: &[NodeIndex],
    unknown: &str,
) -> Option<String> {
    let names: Vec<String> = field_nodes
        .iter()
        .map(|&f| str_interner::get(ast.node(ast.fixed_children(f)[0]).str_id))
        .collect();
    did_you_mean(unknown, names.iter().map(String::as_str))
}

/// Suggest similar parameter names for named argument calls.
pub(crate) fn unknown_parameter_help(
    known_names: impl IntoIterator<Item = StrId>,
    unknown: &str,
) -> Option<String> {
    let names: Vec<String> = known_names.into_iter().map(str_interner::get).collect();
    suggest_similar(unknown, names.iter().map(String::as_str))
        .map(|found| format!("did you mean parameter `{found}`?"))
}

/// Actionable `help:` hints for `type mismatch: expected E, found A`.
pub(crate) fn type_mismatch_helps(
    r: &Resolver<'_>,
    ast: &Ast,
    value: NodeIndex,
    actual: TypeIndex,
    expected: TypeIndex,
    context: &str,
) -> Vec<String> {
    let mut helps = Vec::new();
    let (Some(act_canon), Some(exp_canon)) = (
        r.type_pool.canonical_type(actual),
        r.type_pool.canonical_type(expected),
    ) else {
        return helps;
    };

    // 1. `Future(T)` supplied where `T` is expected -> missing `.await`.
    if let TypeKind::Future { inner } = r.type_pool.get(act_canon).kind
        && (r.type_pool.is_subtype(inner, exp_canon)
            || r.type_pool.is_gradually_consistent(inner, exp_canon))
    {
        helps.push(format!(
            "await the `Future` with `.await` to obtain `{}`",
            type_display_name(&r.type_pool, inner)
        ));
    }

    // 2. `!Err T` supplied where `T` is expected -> missing `!` or `! { ... }`.
    if let TypeKind::ErrorQualified { inner, .. } = r.type_pool.get(act_canon).kind
        && !matches!(
            r.type_pool.get(exp_canon).kind,
            TypeKind::ErrorQualified { .. }
        )
        && (r.type_pool.is_subtype(inner, exp_canon)
            || r.type_pool.is_gradually_consistent(inner, exp_canon))
    {
        helps.push(
            "propagate errors with `expr!` (in a function returning `!Err ...`) or handle them with `expr ! { error e => ..., v! => ... }`"
                .into(),
        );
    }

    // 3. `?T` supplied where non-optional `T` is expected -> missing `?` or null check.
    if let TypeKind::Optional { inner } = r.type_pool.get(act_canon).kind
        && !matches!(r.type_pool.get(exp_canon).kind, TypeKind::Optional { .. })
        && (r.type_pool.is_subtype(inner, exp_canon)
            || r.type_pool.is_gradually_consistent(inner, exp_canon))
    {
        helps.push(
            "propagate `null` with `expr?`, unwrap with `expr.unwrap()`, or narrow with `if x != null { ... }`"
                .into(),
        );
    }

    // 4. Numeric conversion (`f64 -> i64`, narrowing `i64 -> i32`, signed <-> unsigned).
    if let (Some(act_intr), Some(exp_intr)) = (
        r.type_pool.as_intrinsic(act_canon),
        r.type_pool.as_intrinsic(exp_canon),
    ) {
        if act_intr.is_numeric() && exp_intr.is_numeric() {
            helps.push(format!(
                "numeric narrowing or signedness/float conversion requires an explicit cast: `.as({})`",
                exp_intr.name()
            ));
        } else if exp_intr == Intrinsic::Bool && context.contains("condition") {
            helps.push(
                "conditions in Nessa must have type `bool`; write an explicit comparison such as `x != 0` or `x != null`"
                    .into(),
            );
        } else if act_intr == Intrinsic::Str
            && exp_intr == Intrinsic::Char
            && !value.is_null()
            && ast.node(value).kind == NodeKind::Str
        {
            let raw = str_interner::get(ast.node(value).str_id);
            if raw.chars().count() == 3 {
                let ch = raw.chars().nth(1).unwrap_or('a');
                helps.push(format!(
                    "use single quotes `'{ch}'` for a `char` literal (`\"...\"` is a `String`)"
                ));
            }
        }
    }

    // 5. `xs.append(elem)` where `List` is expected (`append` concatenates another `List`, `push` adds one element).
    if r.type_pool.collection_role(exp_canon) == Some(CollectionRole::List)
        && r.type_pool.collection_role(act_canon) != Some(CollectionRole::List)
        && is_argument_of_list_append(r, ast, value)
    {
        helps.push(
            "`List.append(other)` appends all elements of another `List`; to append a single element, use `.push(value)`"
                .into(),
        );
    }

    helps
}

fn is_argument_of_list_append(r: &Resolver<'_>, ast: &Ast, arg_node: NodeIndex) -> bool {
    if arg_node.is_null() {
        return false;
    }
    for (idx, node) in ast.nodes.iter().enumerate() {
        if node.kind != NodeKind::Call {
            continue;
        }
        let call_idx = NodeIndex(idx as u32);
        if !ast.multi_children(call_idx).contains(&arg_node) {
            continue;
        }
        let callee = node.children[0];
        if !callee.is_null() && ast.node(callee).kind == NodeKind::Projection {
            let proj_children = ast.fixed_children(callee);
            let member = proj_children[1];
            if !member.is_null()
                && str_interner::get(ast.node(member).str_id) == "append"
                && r.node_types.get(&proj_children[0]).is_some_and(|&ty| {
                    r.type_pool.collection_role(ty) == Some(CollectionRole::List)
                })
            {
                return true;
            }
        }
    }
    false
}

/// Actionable `help:` hints for binary arithmetic misuse (`String + String`, `List + List`, etc.).
pub(crate) fn arithmetic_misuse_helps(
    r: &Resolver<'_>,
    op: NodeKind,
    left: TypeIndex,
    right: TypeIndex,
) -> Vec<String> {
    let mut helps = Vec::new();
    if op == NodeKind::Add {
        let is_str = r.type_pool.as_intrinsic(left) == Some(Intrinsic::Str)
            || r.type_pool.as_intrinsic(right) == Some(Intrinsic::Str);
        if is_str {
            helps.push(
                "Nessa uses `++` (or string interpolation `\"{a}{b}\"`) to concatenate strings, not `+`"
                    .into(),
            );
            return helps;
        }
        let is_list = r.type_pool.collection_role(left) == Some(CollectionRole::List)
            || r.type_pool.collection_role(right) == Some(CollectionRole::List);
        if is_list {
            helps.push("Nessa uses `++` (or `.concat(...)`) to concatenate lists, not `+`".into());
            return helps;
        }
    }
    if matches!(
        r.type_pool
            .canonical_type(left)
            .map(|t| &r.type_pool.get(t).kind),
        Some(TypeKind::Optional { .. })
    ) || matches!(
        r.type_pool
            .canonical_type(right)
            .map(|t| &r.type_pool.get(t).kind),
        Some(TypeKind::Optional { .. })
    ) {
        helps.push(
            "one of the operands is optional (`?T`); unwrap or narrow `null` before arithmetic"
                .into(),
        );
    }
    let _ = SymbolId(0);
    let _ = SymbolKind::Variable;
    helps
}
