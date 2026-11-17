//! Resolution, default-method publication, and prerequisite validation for `derive Json`.

use std::collections::HashSet;

use ast::{Ast, NodeIndex, NodeKind};
use str_interner::StrId;
use type_pool::{DERIVE_FUNC_ID, Intrinsic, TypeIndex, TypeKind};

use crate::resolver::Resolver;
use crate::{ScopeId, SymbolId, SymbolKind, Visibility};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonNirTypes {
    pub err_str_type: TypeIndex,
    pub err_unit_type: TypeIndex,
    pub err_self_type: TypeIndex,
    pub write_helper_fn_type: TypeIndex,
    pub from_json_fn_type: TypeIndex,
    pub read_helper_fn_type: TypeIndex,
    pub tuple_str_i64: TypeIndex,
    pub tuple_i64: TypeIndex,
    pub tuple_str: TypeIndex,
    pub tuple_str2: TypeIndex,
    pub tuple_str3: TypeIndex,
    /// Return tuple of `JSON_TAPE_OPEN`: `(i64, i64, String)`.
    pub tuple_i64_i64_str: TypeIndex,
    /// Return tuple of `JSON_WRITER_FINISH`: `(i64, String, String)`.
    pub tuple_i64_str_str: TypeIndex,
    /// Return tuple of `JSON_TAPE_READ_INT / FLOAT / STR`: `(i64, Any, String)`.
    pub tuple_i64_any_str: TypeIndex,
    /// Return tuple of `JSON_TAPE_READ_BOOL`: `(i64, bool, String)`.
    pub tuple_i64_bool_str: TypeIndex,
    /// Return tuple of `JSON_TAPE_OBJ_SINGLE_VARIANT`: `(i64, String)`.
    pub tuple_i64_str: TypeIndex,
    /// `?String` return type of `JSON_TAPE_OBJ_UNKNOWN`.
    pub opt_str_type: TypeIndex,
    pub struct_known_keys_tuple: TypeIndex,
    pub enum_variant_tuples: Vec<TypeIndex>,
    /// `(canonical_ty, err_c_type, write_c_fn_type, read_c_fn_type)` for nested structural helpers.
    pub component_helper_types: Vec<(TypeIndex, TypeIndex, TypeIndex, TypeIndex)>,
}

impl Default for JsonNirTypes {
    fn default() -> Self {
        Self {
            err_str_type: TypeIndex::INVALID,
            err_unit_type: TypeIndex::INVALID,
            err_self_type: TypeIndex::INVALID,
            write_helper_fn_type: TypeIndex::INVALID,
            from_json_fn_type: TypeIndex::INVALID,
            read_helper_fn_type: TypeIndex::INVALID,
            tuple_str_i64: TypeIndex::INVALID,
            tuple_i64: TypeIndex::INVALID,
            tuple_str: TypeIndex::INVALID,
            tuple_str2: TypeIndex::INVALID,
            tuple_str3: TypeIndex::INVALID,
            tuple_i64_i64_str: TypeIndex::INVALID,
            tuple_i64_str_str: TypeIndex::INVALID,
            tuple_i64_any_str: TypeIndex::INVALID,
            tuple_i64_bool_str: TypeIndex::INVALID,
            tuple_i64_str: TypeIndex::INVALID,
            opt_str_type: TypeIndex::INVALID,
            struct_known_keys_tuple: TypeIndex::INVALID,
            enum_variant_tuples: Vec::new(),
            component_helper_types: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedJsonPlan {
    pub node: NodeIndex,
    pub implementor: TypeIndex,
    pub trait_type: TypeIndex,
    pub method_name: StrId,
    pub signature: TypeIndex,
    pub from_json_symbol: Option<SymbolId>,
    pub json_error_type: TypeIndex,
    pub struct_field_defaults: Vec<(usize, NodeIndex)>,
    /// `(component_ty, from_json_symbol, takes_allow_unknown_bool)` for custom `impl Json` types.
    pub custom_decoders: Vec<(TypeIndex, SymbolId, bool)>,
    pub nir_types: JsonNirTypes,
}

pub(crate) fn error_type(r: &Resolver<'_>) -> Option<TypeIndex> {
    let (ty, _) = crate::trusted::std_type(r, "json", "JsonError")?;
    matches!(r.type_pool.get(ty).kind, TypeKind::Enum { .. }).then_some(ty)
}

fn sig_template_node(r: &Resolver<'_>, ast: &Ast) -> Option<NodeIndex> {
    let sym_id = crate::trusted::std_function(r, "json", "__derived_from_json_sig")?;
    let sym = &r.symbols[sym_id.0 as usize];
    (sym.kind == SymbolKind::Function
        && !sym.def_node.is_null()
        && ast.node(sym.def_node).kind == NodeKind::FunctionDef)
        .then_some(sym.def_node)
}

fn resolve_trait_in_scope(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    scope: ScopeId,
) -> Option<TypeIndex> {
    if node.is_null() {
        return None;
    }
    if ast.node(node).kind == NodeKind::View {
        let children = ast.fixed_children(node);
        let object = children[0];
        if (r.options.builtin_access || r.options.privileged_nodes.contains(&node))
            && !object.is_null()
            && ast.node(object).kind == NodeKind::Symbol
            && str_interner::get(ast.node(children[1]).str_id) == "builtin"
        {
            let name_node = ast.fixed_children(object)[0];
            let name = str_interner::get(ast.node(name_node).str_id);
            if let Some(builtin_sym) = r.resolve_builtin_view(node, &name) {
                return r
                    .type_pool
                    .canonical_type(r.symbols[builtin_sym.0 as usize].type_index);
            }
        }
        return None;
    }
    let sym = crate::associated::expression_symbol_in_scope(r, ast, node, scope)?;
    let kind = r.symbols[sym.0 as usize].kind;
    let ty = r.symbols[sym.0 as usize].type_index;
    if kind == SymbolKind::Trait {
        return r.type_pool.canonical_type(ty);
    }
    crate::associated::target_type(r, ast, sym)
}

fn resolve_target_in_scope(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    scope: ScopeId,
) -> Option<TypeIndex> {
    if node.is_null() {
        return None;
    }
    if matches!(ast.node(node).kind, NodeKind::Tuple | NodeKind::Call) {
        let prev = r.current_scope;
        r.current_scope = scope;
        let resolved = crate::typing::resolve_type_expr_inner(r, ast, node)
            .and_then(|t| r.type_pool.canonical_type(t));
        r.current_scope = prev;
        return resolved;
    }
    let sym = crate::associated::expression_symbol_in_scope(r, ast, node, scope)?;
    crate::associated::target_type(r, ast, sym)
}

pub(crate) fn attach_pending_derives(r: &mut Resolver<'_>, ast: &Ast) -> bool {
    let pending = r.pending_derives.clone();
    let mut changed = false;
    for (scope, derive_node) in pending {
        changed |= attach_derive_associated(r, ast, scope, derive_node);
    }
    changed
}

pub(crate) fn attach_derive_associated(
    r: &mut Resolver<'_>,
    ast: &Ast,
    parent_scope: ScopeId,
    derive_node: NodeIndex,
) -> bool {
    if derive_node.is_null()
        || ast.node(derive_node).kind != NodeKind::DeriveDef
        || r.attached_json_derives.contains(&derive_node)
    {
        return false;
    }
    let json_trait = r.type_pool.well_known.json;
    let has_json = ast
        .multi_children(derive_node)
        .iter()
        .any(|&tr| resolve_trait_in_scope(r, ast, tr, parent_scope) == Some(json_trait));
    if !has_json {
        return false;
    }
    let target_node = ast.fixed_children(derive_node)[0];
    let Some(ty) = resolve_target_in_scope(r, ast, target_node, parent_scope) else {
        return false;
    };
    if !matches!(
        r.type_pool.get(ty).kind,
        TypeKind::Struct { .. }
            | TypeKind::Enum { .. }
            | TypeKind::Tuple { .. }
            | TypeKind::Newtype { .. }
    ) {
        return false;
    }
    r.attached_json_derives.insert(derive_node);
    let from_json_name = str_interner::intern("from_json");
    let already_has_from_json = r.scopes.iter().any(|s| {
        s.assoc_type == Some(ty)
            && !r.imports.extension_scopes.contains_key(&s.id)
            && s.bindings.contains_key(&from_json_name)
    });
    if already_has_from_json || r.derived_from_json_symbols.contains_key(&ty) {
        return false;
    }
    let prev = r.current_scope;
    r.current_scope = parent_scope;
    let _assoc_scope = r.push_scope(NodeIndex::NULL, Some(ty));
    let sym_id = r.define_symbol(
        from_json_name,
        SymbolKind::Function,
        NodeIndex::NULL,
        Visibility::Public,
    );
    r.pop_scope();
    r.current_scope = prev;
    r.derived_from_json_symbols.insert(ty, sym_id);
    true
}

pub(crate) fn prepare_signatures(r: &mut Resolver<'_>, ast: &Ast) {
    let Some(err_ty) = error_type(r) else {
        return;
    };
    let tmpl = sig_template_node(r, ast);
    if let Some(decl) = tmpl {
        for &param in ast.multi_children(decl) {
            if ast.node(param).kind == NodeKind::ParamOptional {
                let default_expr = ast.fixed_children(param)[2];
                if !default_expr.is_null() {
                    r.node_types
                        .insert(default_expr, Intrinsic::Bool.type_index());
                }
            }
        }
    }
    let entries: Vec<(TypeIndex, SymbolId)> = r
        .derived_from_json_symbols
        .iter()
        .map(|(&ty, &sym)| (ty, sym))
        .collect();
    for (ty, sym_id) in entries {
        let Ok(ret) = r.type_pool.normalize_error_type(vec![err_ty], ty) else {
            continue;
        };
        let fn_ty = r.register_type(TypeKind::Function {
            params: vec![Intrinsic::Str.type_index(), Intrinsic::Bool.type_index()],
            ret,
        });
        r.symbols[sym_id.0 as usize].type_index = fn_ty;
        if let Some(decl) = tmpl {
            r.synthetic_call_templates.insert(sym_id, decl);
        }
    }
}

pub(crate) fn default_template(r: &Resolver<'_>, name: StrId) -> Option<SymbolId> {
    if error_type(r).is_none()
        || (name != str_interner::intern("to_json_pretty")
            && name != str_interner::intern("to_json_string"))
    {
        return None;
    }
    r.symbols
        .iter()
        .find(|symbol| {
            symbol.name == name
                && symbol.kind == SymbolKind::Function
                && r.options.privileged_nodes.contains(&symbol.def_node)
                && r.scopes[symbol.scope.0 as usize].assoc_type == Some(r.type_pool.well_known.json)
        })
        .map(|symbol| symbol.id)
}

pub(crate) fn complete_records(r: &mut Resolver<'_>, ast: &Ast) {
    if error_type(r).is_none() {
        return;
    }
    for method_str in ["to_json_pretty", "to_json_string"] {
        let name = str_interner::intern(method_str);
        let Some(declaration) = default_template(r, name) else {
            continue;
        };
        let provider = r.symbols[declaration.0 as usize].scope.0;
        let records = r.type_pool.trait_impls_snapshot().to_vec();
        for record in records {
            if record.trait_type != r.type_pool.well_known.json {
                continue;
            }
            if record.methods.iter().any(|method| method.name == name) {
                continue;
            }
            let function = match crate::default_methods::register(
                r,
                ast,
                declaration,
                record.implementor,
                record.trait_type,
                record.visible_scope,
                provider,
            ) {
                Ok(function) => function,
                Err(error) => {
                    r.diag_ctx
                        .error(error)
                        .with_primary_span(
                            ast.node(r.symbols[declaration.0 as usize].def_node).span,
                        )
                        .emit(r.diag_ctx);
                    continue;
                }
            };
            let slot = type_pool::MethodSlot {
                access: type_pool::MethodAccess::Public,
                name,
                func_id: function.0,
                trait_impl: Some(record.trait_type),
                visible_scope: record.visible_scope,
            };
            if let Err(error) = r.type_pool.add_trait_method(
                record.implementor,
                record.trait_type,
                record.visible_scope,
                slot.clone(),
            ) {
                r.diag_ctx
                    .error(format!("cannot publish Json default: {error}"))
                    .emit(r.diag_ctx);
                continue;
            }
            r.type_pool.add_method(record.implementor, slot);
        }
    }
}

pub(crate) fn register_json(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    implementor: TypeIndex,
) -> bool {
    let Some(json_error_type) = error_type(r) else {
        report(
            r,
            ast,
            node,
            "Json derivation requires the trusted std.json.JsonError interface",
        );
        return false;
    };
    let Ok(ret) = r
        .type_pool
        .normalize_error_type(vec![json_error_type], Intrinsic::Str.type_index())
    else {
        return false;
    };
    let signature = r.type_pool.intern_structural(TypeKind::Function {
        params: vec![implementor],
        ret,
    });
    let from_json_symbol = r.derived_from_json_symbols.get(&implementor).copied();
    let struct_field_defaults = collect_struct_field_defaults(r, ast, implementor);
    r.json_derivations.push(DerivedJsonPlan {
        node,
        implementor,
        trait_type: r.type_pool.well_known.json,
        method_name: str_interner::intern("to_json"),
        signature,
        from_json_symbol,
        json_error_type,
        struct_field_defaults,
        custom_decoders: Vec::new(),
        nir_types: JsonNirTypes::default(),
    });
    true
}

fn collect_struct_field_defaults(
    r: &Resolver<'_>,
    ast: &Ast,
    implementor: TypeIndex,
) -> Vec<(usize, NodeIndex)> {
    let Some(struct_def) = r.symbols.iter().find_map(|sym| {
        (!sym.def_node.is_null()
            && ast.node(sym.def_node).kind == NodeKind::StructDef
            && r.type_pool.canonical_type(sym.type_index) == Some(implementor))
        .then_some(sym.def_node)
    }) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (idx, field) in crate::structs::fields(ast, struct_def)
        .into_iter()
        .enumerate()
    {
        let default_expr = ast.fixed_children(field)[2];
        if !default_expr.is_null() {
            out.push((idx, default_expr));
        }
    }
    out
}

pub(crate) fn declared_signature(
    r: &mut Resolver<'_>,
    ast: &Ast,
    receiver: TypeIndex,
    name: StrId,
) -> Option<TypeIndex> {
    if name != str_interner::intern("to_json") {
        return None;
    }
    let err_ty = error_type(r)?;
    let receiver = r.type_pool.canonical_type(receiver)?;
    let denotes = |node: NodeIndex| {
        r.node_type_values
            .get(&node)
            .copied()
            .or_else(|| {
                r.node_symbols
                    .get(&node)
                    .map(|id| r.symbols[id.0 as usize].type_index)
            })
            .and_then(|ty| r.type_pool.canonical_type(ty))
    };
    let has_derive = (1..ast.nodes.len()).any(|raw| {
        let idx = NodeIndex(raw as u32);
        ast.node(idx).kind == NodeKind::DeriveDef
            && denotes(ast.fixed_children(idx)[0]) == Some(receiver)
            && ast
                .multi_children(idx)
                .iter()
                .any(|&node| denotes(node) == Some(r.type_pool.well_known.json))
    });
    if !has_derive {
        return None;
    }
    let ret = r
        .type_pool
        .normalize_error_type(vec![err_ty], Intrinsic::Str.type_index())
        .ok()?;
    Some(r.register_type(TypeKind::Function {
        params: Vec::new(),
        ret,
    }))
}

pub(crate) fn validate(r: &mut Resolver<'_>, ast: &Ast) {
    let plans = r.json_derivations.clone();
    let mut updated_decoders = Vec::with_capacity(plans.len());

    for plan in &plans {
        let mut seen = HashSet::from([plan.implementor]);
        let mut decoders = Vec::new();
        match r.type_pool.get(plan.implementor).kind.clone() {
            TypeKind::Struct { fields, .. } => {
                for field in fields {
                    if let Err(msg) = check_json_component(
                        r,
                        field.ty,
                        plan.json_error_type,
                        "field",
                        &mut seen,
                        &mut decoders,
                    ) {
                        report(r, ast, plan.node, msg);
                    }
                }
            }
            TypeKind::Enum { variants, .. } => {
                for variant in variants {
                    for field in variant.fields {
                        if let Err(msg) = check_json_component(
                            r,
                            field.ty,
                            plan.json_error_type,
                            "variant field",
                            &mut seen,
                            &mut decoders,
                        ) {
                            report(r, ast, plan.node, msg);
                        }
                    }
                }
            }
            TypeKind::Tuple { elements } => {
                for elem in elements {
                    if let Err(msg) = check_json_component(
                        r,
                        elem,
                        plan.json_error_type,
                        "tuple element",
                        &mut seen,
                        &mut decoders,
                    ) {
                        report(r, ast, plan.node, msg);
                    }
                }
            }
            TypeKind::Newtype { inner, .. } => {
                if let Err(msg) = check_json_component(
                    r,
                    inner,
                    plan.json_error_type,
                    "newtype inner",
                    &mut seen,
                    &mut decoders,
                ) {
                    report(r, ast, plan.node, msg);
                }
            }
            _ => {
                report(
                    r,
                    ast,
                    plan.node,
                    "Json derivation supports only struct, enum, tuple typealias, and newtype targets",
                );
            }
        }
        updated_decoders.push(decoders);
    }

    for (plan, decoders) in r.json_derivations.iter_mut().zip(updated_decoders) {
        plan.custom_decoders = decoders;
    }

    if !r.diag_ctx.has_errors() {
        prepare_nir_types(r);
    }
}

fn prepare_nir_types(r: &mut Resolver<'_>) {
    let plans = r.json_derivations.clone();
    let mut built = Vec::with_capacity(plans.len());

    for plan in &plans {
        let err_ty = plan.json_error_type;
        let str_ty = Intrinsic::Str.type_index();
        let i64_ty = Intrinsic::I64.type_index();
        let bool_ty = Intrinsic::Bool.type_index();
        let unit_ty = Intrinsic::Unit.type_index();
        let any_ty = Intrinsic::Any.type_index();

        let err_str_type = r
            .type_pool
            .normalize_error_type(vec![err_ty], str_ty)
            .expect("checked JsonError Str envelope");
        let err_unit_type = r
            .type_pool
            .normalize_error_type(vec![err_ty], unit_ty)
            .expect("checked JsonError Unit envelope");
        let err_self_type = r
            .type_pool
            .normalize_error_type(vec![err_ty], plan.implementor)
            .expect("checked JsonError implementor envelope");
        let write_helper_fn_type = r.register_type(TypeKind::Function {
            params: vec![plan.implementor, i64_ty],
            ret: err_unit_type,
        });
        let from_json_fn_type = r.register_type(TypeKind::Function {
            params: vec![str_ty, bool_ty],
            ret: err_self_type,
        });
        let read_helper_fn_type = r.register_type(TypeKind::Function {
            params: vec![i64_ty, i64_ty, str_ty, bool_ty],
            ret: err_self_type,
        });

        let tuple_str_i64 = r.register_type(TypeKind::Tuple {
            elements: vec![str_ty, i64_ty],
        });
        let tuple_i64 = r.register_type(TypeKind::Tuple {
            elements: vec![i64_ty],
        });
        let tuple_str = r.register_type(TypeKind::Tuple {
            elements: vec![str_ty],
        });
        let tuple_str2 = r.register_type(TypeKind::Tuple {
            elements: vec![str_ty, str_ty],
        });
        let tuple_str3 = r.register_type(TypeKind::Tuple {
            elements: vec![str_ty, str_ty, str_ty],
        });
        let tuple_i64_i64_str = r.register_type(TypeKind::Tuple {
            elements: vec![i64_ty, i64_ty, str_ty],
        });
        let tuple_i64_str_str = r.register_type(TypeKind::Tuple {
            elements: vec![i64_ty, str_ty, str_ty],
        });
        let tuple_i64_any_str = r.register_type(TypeKind::Tuple {
            elements: vec![i64_ty, any_ty, str_ty],
        });
        let tuple_i64_bool_str = r.register_type(TypeKind::Tuple {
            elements: vec![i64_ty, bool_ty, str_ty],
        });
        let tuple_i64_str = r.register_type(TypeKind::Tuple {
            elements: vec![i64_ty, str_ty],
        });
        let opt_str_type = r.register_type(TypeKind::Optional { inner: str_ty });

        let mut struct_known_keys_tuple = unit_ty;
        let mut enum_variant_tuples = Vec::new();
        let mut component_tys = Vec::new();
        let mut visited_components = HashSet::from([plan.implementor]);

        match r.type_pool.get(plan.implementor).kind.clone() {
            TypeKind::Struct { fields, .. } => {
                if !fields.is_empty() {
                    struct_known_keys_tuple = r.register_type(TypeKind::Tuple {
                        elements: vec![str_ty; fields.len()],
                    });
                }
                for f in fields {
                    collect_structural_helper_types(
                        r,
                        f.ty,
                        &mut visited_components,
                        &mut component_tys,
                    );
                }
            }
            TypeKind::Enum { variants, .. } => {
                for v in variants {
                    if v.fields.is_empty() {
                        enum_variant_tuples.push(unit_ty);
                    } else {
                        let f_tys: Vec<_> = v.fields.iter().map(|f| f.ty).collect();
                        let vtup = r.register_type(TypeKind::Tuple { elements: f_tys });
                        enum_variant_tuples.push(vtup);
                        for f in v.fields {
                            collect_structural_helper_types(
                                r,
                                f.ty,
                                &mut visited_components,
                                &mut component_tys,
                            );
                        }
                    }
                }
            }
            TypeKind::Tuple { elements } => {
                for elem in elements {
                    collect_structural_helper_types(
                        r,
                        elem,
                        &mut visited_components,
                        &mut component_tys,
                    );
                }
            }
            TypeKind::Newtype { inner, .. } => {
                collect_structural_helper_types(
                    r,
                    inner,
                    &mut visited_components,
                    &mut component_tys,
                );
            }
            _ => {}
        }

        let mut component_helper_types = Vec::with_capacity(component_tys.len());
        for c_ty in component_tys {
            let Ok(err_c) = r.type_pool.normalize_error_type(vec![err_ty], c_ty) else {
                continue;
            };
            let write_c = r.register_type(TypeKind::Function {
                params: vec![c_ty, i64_ty],
                ret: err_unit_type,
            });
            let read_c = r.register_type(TypeKind::Function {
                params: vec![i64_ty, i64_ty, str_ty, bool_ty],
                ret: err_c,
            });
            component_helper_types.push((c_ty, err_c, write_c, read_c));
        }

        built.push(JsonNirTypes {
            err_str_type,
            err_unit_type,
            err_self_type,
            write_helper_fn_type,
            from_json_fn_type,
            read_helper_fn_type,
            tuple_str_i64,
            tuple_i64,
            tuple_str,
            tuple_str2,
            tuple_str3,
            tuple_i64_i64_str,
            tuple_i64_str_str,
            tuple_i64_any_str,
            tuple_i64_bool_str,
            tuple_i64_str,
            opt_str_type,
            struct_known_keys_tuple,
            enum_variant_tuples,
            component_helper_types,
        });
    }

    for (plan, nir_types) in r.json_derivations.iter_mut().zip(built) {
        plan.nir_types = nir_types;
    }
}

fn collect_structural_helper_types(
    r: &Resolver<'_>,
    ty: TypeIndex,
    visited: &mut HashSet<TypeIndex>,
    out: &mut Vec<TypeIndex>,
) {
    let Some(canon) = r.type_pool.canonical_type(ty) else {
        return;
    };
    if !visited.insert(canon) {
        return;
    }
    if r.json_derivations.iter().any(|p| p.implementor == canon) {
        return;
    }
    match r.type_pool.get(canon).kind.clone() {
        TypeKind::Optional { inner } => {
            out.push(canon);
            collect_structural_helper_types(r, inner, visited, out);
        }
        TypeKind::Tuple { elements } => {
            out.push(canon);
            for elem in elements {
                collect_structural_helper_types(r, elem, visited, out);
            }
        }
        _ => {
            out.push(canon);
        }
    }
}

fn check_json_component(
    r: &Resolver<'_>,
    ty: TypeIndex,
    err_ty: TypeIndex,
    role: &str,
    seen: &mut HashSet<TypeIndex>,
    decoders: &mut Vec<(TypeIndex, SymbolId, bool)>,
) -> Result<(), String> {
    let Some(canon) = r.type_pool.canonical_type(ty) else {
        return Err(format!(
            "Json derivation cannot resolve {role} type `<unknown>`"
        ));
    };
    let ty_name = crate::typing::type_display_name(&r.type_pool, ty);

    if r.type_pool.is_reserved_collection_role(canon) {
        return Err(format!(
            "Json derivation does not support untyped runtime {role} type `{ty_name}`"
        ));
    }
    if ty_name == "Future" || ty_name.starts_with("Future(") {
        return Err(format!(
            "Json derivation does not support {role} type `{ty_name}`"
        ));
    }
    if let Some(intr) = r.type_pool.as_intrinsic(canon) {
        if matches!(
            intr,
            Intrinsic::Any
                | Intrinsic::Closure
                | Intrinsic::Continuation
                | Intrinsic::NoReturn
                | Intrinsic::Type
        ) {
            return Err(format!(
                "Json derivation does not support {role} type `{ty_name}`"
            ));
        }
        if checked_to_json_method(r, canon, err_ty) {
            return Ok(());
        }
    }
    if matches!(
        r.type_pool.get(canon).kind,
        TypeKind::Function { .. }
            | TypeKind::Effect { .. }
            | TypeKind::Trait { .. }
            | TypeKind::Module { .. }
            | TypeKind::ErrorQualified { .. }
            | TypeKind::EffectQualified { .. }
    ) {
        return Err(format!(
            "Json derivation does not support {role} type `{ty_name}`"
        ));
    }
    if seen.contains(&canon)
        || r.json_derivations
            .iter()
            .any(|plan| plan.implementor == canon)
    {
        return Ok(());
    }
    match r.type_pool.get(canon).kind.clone() {
        TypeKind::Optional { inner } => {
            if r.type_pool.as_intrinsic(inner) == Some(Intrinsic::NoReturn) {
                return Ok(());
            }
            let mut next = seen.clone();
            next.insert(canon);
            return check_json_component(r, inner, err_ty, role, &mut next, decoders);
        }
        TypeKind::Tuple { elements } => {
            if checked_to_json_method(r, canon, err_ty)
                && let Some((sym, takes_bool)) = checked_custom_from_json(r, canon, err_ty)
            {
                record_decoder(decoders, canon, sym, takes_bool);
                return Ok(());
            }
            for elem in elements {
                let mut next = seen.clone();
                next.insert(canon);
                check_json_component(r, elem, err_ty, role, &mut next, decoders)?;
            }
            return Ok(());
        }
        _ => {}
    }

    if checked_to_json_method(r, canon, err_ty) {
        if let Some((sym, takes_bool)) = checked_custom_from_json(r, canon, err_ty) {
            record_decoder(decoders, canon, sym, takes_bool);
            return Ok(());
        }
        return Err(format!(
            "Json derivation requires an associated `{ty_name}.from_json(String) -> !JsonError {ty_name}` decoder for custom Json {role} type `{ty_name}`"
        ));
    }

    let has_scoped_extend = r.type_pool.trait_impls_snapshot().iter().any(|rec| {
        rec.implementor == canon
            && rec.trait_type == r.type_pool.well_known.json
            && rec.visible_scope.is_some()
    });
    if has_scoped_extend {
        return Err(format!(
            "Json derivation requires a global `impl Json` for {role} type `{ty_name}`; local `extend Json` implementations cannot satisfy `derive Json`"
        ));
    }

    Err(format!(
        "Json derivation requires a checked global Json implementation for {role} type `{ty_name}`"
    ))
}

fn record_decoder(
    decoders: &mut Vec<(TypeIndex, SymbolId, bool)>,
    ty: TypeIndex,
    sym: SymbolId,
    takes_bool: bool,
) {
    if !decoders.iter().any(|&(existing, _, _)| existing == ty) {
        decoders.push((ty, sym, takes_bool));
    }
}

fn checked_to_json_method(r: &Resolver<'_>, owner: TypeIndex, err_ty: TypeIndex) -> bool {
    let Some(slot) = r.type_pool.find_trait_method(
        owner,
        r.type_pool.well_known.json,
        str_interner::intern("to_json"),
    ) else {
        return false;
    };
    if slot.func_id == DERIVE_FUNC_ID {
        return r
            .json_derivations
            .iter()
            .any(|plan| plan.implementor == owner);
    }
    let Some(sym) = r.symbols.get(slot.func_id as usize) else {
        return false;
    };
    if sym.kind != SymbolKind::Function || sym.type_index == TypeIndex::INVALID {
        return false;
    }
    let Some(sig) = r.type_pool.canonical_type(sym.type_index) else {
        return false;
    };
    match &r.type_pool.get(sig).kind {
        TypeKind::Function { params, ret } => {
            params.len() == 1
                && r.type_pool.canonical_type(params[0]) == Some(owner)
                && matches_json_error_return(r, *ret, err_ty, Intrinsic::Str.type_index())
        }
        _ => false,
    }
}

fn checked_custom_from_json(
    r: &Resolver<'_>,
    owner: TypeIndex,
    err_ty: TypeIndex,
) -> Option<(SymbolId, bool)> {
    let from_json_name = str_interner::intern("from_json");
    for scope in &r.scopes {
        if scope.assoc_type != Some(owner) || r.imports.extension_scopes.contains_key(&scope.id) {
            continue;
        }
        let Some(&sym_id) = scope.bindings.get(&from_json_name) else {
            continue;
        };
        let sym = &r.symbols[sym_id.0 as usize];
        if sym.kind != SymbolKind::Function || sym.type_index == TypeIndex::INVALID {
            continue;
        }
        let Some(sig) = r.type_pool.canonical_type(sym.type_index) else {
            continue;
        };
        if let TypeKind::Function { params, ret } = &r.type_pool.get(sig).kind
            && matches_json_error_return(r, *ret, err_ty, owner)
        {
            if params.len() == 2
                && r.type_pool.canonical_type(params[0]) == Some(Intrinsic::Str.type_index())
                && r.type_pool.canonical_type(params[1]) == Some(Intrinsic::Bool.type_index())
            {
                return Some((sym_id, true));
            }
            if params.len() == 1
                && r.type_pool.canonical_type(params[0]) == Some(Intrinsic::Str.type_index())
            {
                return Some((sym_id, false));
            }
        }
    }
    None
}

fn matches_json_error_return(
    r: &Resolver<'_>,
    ret: TypeIndex,
    err_ty: TypeIndex,
    expected_inner: TypeIndex,
) -> bool {
    let Some(canon_ret) = r.type_pool.canonical_type(ret) else {
        return false;
    };
    match &r.type_pool.get(canon_ret).kind {
        TypeKind::ErrorQualified { errors, inner } => {
            errors.len() == 1
                && r.type_pool.canonical_type(errors[0]) == Some(err_ty)
                && r.type_pool.canonical_type(*inner) == Some(expected_inner)
        }
        _ => false,
    }
}

fn report(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, message: impl Into<String>) {
    r.diag_ctx
        .error(message.into())
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
}
