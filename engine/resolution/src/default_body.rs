//! Concrete checking contexts and isolated facts for shared default-body ASTs.

use std::collections::{HashMap, HashSet};

use ast::{Ast, NodeIndex};
use type_pool::{TraitMethodKey, TraitTypeStep, TypeIndex};

use crate::{SymbolId, resolver::Resolver};

pub(crate) fn requires_replay(r: &Resolver<'_>, ast: &Ast, root: NodeIndex) -> bool {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if node.is_null() {
            continue;
        }
        if matches!(
            ast.node(node).kind,
            ast::NodeKind::ForLoop
                | ast::NodeKind::ErrorQualifiedType
                | ast::NodeKind::ErrorConstruction
                | ast::NodeKind::ErrorPropagation
                | ast::NodeKind::ErrorElimination
        ) {
            return true;
        }
        if r.node_symbols.get(&node).is_some_and(|symbol| {
            let ty = r.symbols[symbol.0 as usize].type_index;
            r.type_pool.canonical_type(ty).is_some() && r.type_pool.contains_associated_type(ty)
        }) {
            return true;
        }
        if matches!(
            ast.node(node).kind,
            ast::NodeKind::Call | ast::NodeKind::PatternCall
        ) && crate::type_factories::identity(r, ast.fixed_children(node)[0]).is_some()
        {
            return true;
        }
        pending.extend(ast.fixed_children(node));
        pending.extend(ast.multi_children(node));
    }
    false
}

pub(crate) struct DefaultBodyContext {
    pub implementor: TypeIndex,
    pub trait_owner: TypeIndex,
    pub implementation_trait: TypeIndex,
    pub scope: Option<u32>,
    pub bindings: Vec<type_pool::AssociatedTypeBinding>,
    pub self_value_paths: HashMap<NodeIndex, Vec<Vec<TraitTypeStep>>>,
}

pub(crate) fn specialize_body_annotation(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    ty: TypeIndex,
) -> Option<TypeIndex> {
    let Some(context) = &r.default_body_context else {
        return Some(ty);
    };
    let bindings = context.bindings.clone();
    let owner = context.trait_owner;
    let concrete = context.implementor;
    let ty = match r.type_pool.specialize_associated_type(ty, &bindings) {
        Ok(ty) => ty,
        Err(error) => {
            r.diag_ctx
                .error(error.to_string())
                .with_primary_span(ast.node(node).span)
                .emit(r.diag_ctx);
            return None;
        }
    };
    let paths = match crate::trait_signatures::annotation_self_paths(r, ast, node, owner) {
        Ok(paths) => paths,
        Err(error) => {
            r.diag_ctx
                .error(error)
                .with_primary_span(ast.node(node).span)
                .emit(r.diag_ctx);
            return None;
        }
    };
    let paths: Vec<_> = paths
        .into_iter()
        .filter(|path| crate::trait_signatures::type_at_path(r, ty, path) == Some(owner))
        .map(|path| {
            std::iter::once(TraitTypeStep::Parameter(0))
                .chain(path)
                .collect()
        })
        .collect();
    if paths.is_empty() {
        return Some(ty);
    }
    let declaration = r
        .type_pool
        .intern_structural(type_pool::TypeKind::Function {
            params: vec![ty],
            ret: type_pool::Intrinsic::Unit.type_index(),
        });
    let key = TraitMethodKey {
        trait_owner: owner,
        name: str_interner::intern("__default_annotation"),
        signature: Some(type_pool::TraitMethodSignature {
            declaration,
            self_paths: paths,
            associated_paths: Vec::new(),
            parameter_kinds: vec![type_pool::TraitParameterKind::Required],
        }),
    };
    match r
        .type_pool
        .instantiate_trait_method_signature(&key, concrete)
    {
        Ok(signature) => {
            let type_pool::TypeKind::Function { params, .. } = &r.type_pool.get(signature).kind
            else {
                return None;
            };
            Some(params[0])
        }
        Err(error) => {
            r.diag_ctx
                .error(error.to_string())
                .with_primary_span(ast.node(node).span)
                .emit(r.diag_ctx);
            None
        }
    }
}

/// Body values keep the source Self view for frozen virtual dispatch. Actual
/// function metadata is specialized separately from these lexical bindings.
pub(crate) fn parameter_body_type(
    r: &mut Resolver<'_>,
    ast: &Ast,
    annotation: NodeIndex,
    concrete: TypeIndex,
) -> Option<TypeIndex> {
    let Some(context) = r.default_body_context.take() else {
        return Some(concrete);
    };
    let raw = crate::typing::resolve_type_expr_inner(r, ast, annotation);
    let bindings = context.bindings.clone();
    r.default_body_context = Some(context);
    let raw = raw?;
    if r.type_pool.error_shape(concrete).ok().flatten().is_some() {
        // Qualified parameters carry concrete payloads inside the managed Error
        // ABI; their nested Self positions use replay rather than bare proof locals.
        r.node_type_values.insert(annotation, concrete);
        return Some(concrete);
    }
    match r.type_pool.specialize_associated_type(raw, &bindings) {
        Ok(ty) => Some(ty),
        Err(error) => {
            r.diag_ctx
                .error(error.to_string())
                .with_primary_span(ast.node(annotation).span)
                .emit(r.diag_ctx);
            None
        }
    }
}

pub(crate) fn contextual_parameter_type(
    r: &mut Resolver<'_>,
    ast: &Ast,
    parameter: NodeIndex,
    actual: TypeIndex,
) -> Option<TypeIndex> {
    let Some(context) = &r.default_body_context else {
        return Some(actual);
    };
    let paths = context
        .self_value_paths
        .get(&parameter)
        .cloned()
        .unwrap_or_default();
    if paths.is_empty() {
        return Some(actual);
    }
    let owner = context.trait_owner;
    let concrete = context.implementor;
    // Nested Self parameter paths require a separate carrier and are rejected
    // before a default body is instantiated. Only a direct value keeps a view.
    if paths.iter().any(|path| !path.is_empty()) {
        if paths.iter().any(|path| {
            !path.is_empty()
                && !path.iter().any(|step| {
                    matches!(
                        step,
                        TraitTypeStep::ErrorInner | TraitTypeStep::ErrorMember(_)
                    )
                })
        }) {
            r.diag_ctx.error("associated default methods cannot carry Self evidence inside a parameter type; use a direct Self parameter".into()).with_primary_span(ast.node(parameter).span).emit(r.diag_ctx);
            return None;
        }
        // Nested values are checked and lowered in the concrete replay context.
        // Only direct Self parameters use the separate bare-trait proof ABI.
        return Some(actual);
    }
    let canonical = r.type_pool.canonical_type(actual);
    if canonical == r.type_pool.canonical_type(concrete) || canonical == Some(owner) {
        Some(owner)
    } else {
        r.diag_ctx
            .error("default Self parameter does not match its concrete implementation".into())
            .with_primary_span(ast.node(parameter).span)
            .emit(r.diag_ctx);
        None
    }
}

pub(crate) fn checked_self_value_type(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    actual: TypeIndex,
) -> TypeIndex {
    let Some(context) = &r.default_body_context else {
        return actual;
    };
    let Some(paths) = context.self_value_paths.get(&node).cloned() else {
        return actual;
    };
    let owner = context.trait_owner;
    let concrete = context.implementor;
    let paths: Vec<_> = paths
        .into_iter()
        .filter(|path| crate::trait_signatures::type_at_path(r, actual, path) == Some(owner))
        .map(|path| {
            std::iter::once(TraitTypeStep::Parameter(0))
                .chain(path)
                .collect()
        })
        .collect();
    if paths.is_empty() {
        return actual;
    }
    let declaration = r
        .type_pool
        .intern_structural(type_pool::TypeKind::Function {
            params: vec![actual],
            ret: type_pool::Intrinsic::Unit.type_index(),
        });
    let key = TraitMethodKey {
        trait_owner: owner,
        name: str_interner::intern("__default_value"),
        signature: Some(type_pool::TraitMethodSignature {
            declaration,
            self_paths: paths,
            associated_paths: Vec::new(),
            parameter_kinds: vec![type_pool::TraitParameterKind::Required],
        }),
    };
    let signature = match r
        .type_pool
        .instantiate_trait_method_signature(&key, concrete)
    {
        Ok(signature) => signature,
        Err(error) => {
            r.diag_ctx
                .error(format!("cannot check default Self value: {error}"))
                .with_primary_span(ast.node(node).span)
                .emit(r.diag_ctx);
            return actual;
        }
    };
    let type_pool::TypeKind::Function { params, .. } = &r.type_pool.get(signature).kind else {
        return actual;
    };
    params[0]
}

/// Per-adapter facts from checking a concrete body. Declaration nodes remain
/// shared; field layouts, selected callees and coercions do not.
#[derive(Debug, Clone, Default)]
pub struct DefaultBodyFacts {
    /// Keys absent inside this domain are intentionally absent, not stale base facts.
    pub nodes: HashSet<NodeIndex>,
    pub node_symbols: HashMap<NodeIndex, SymbolId>,
    pub constructor_types: HashMap<NodeIndex, SymbolId>,
    pub enum_variant_indices: HashMap<SymbolId, u32>,
    pub node_field_indices: HashMap<NodeIndex, u32>,
    pub node_type_values: HashMap<NodeIndex, TypeIndex>,
    pub node_coercions: HashMap<NodeIndex, crate::Coercion>,
    pub error_constructions: HashMap<NodeIndex, crate::ErrorConstructionPlan>,
    pub error_conversions: HashMap<NodeIndex, Vec<crate::ErrorConversionPlan>>,
    pub error_propagations: HashMap<NodeIndex, crate::ErrorPropagationPlan>,
    pub error_eliminations: HashMap<NodeIndex, crate::ErrorEliminationPlan>,
    pub error_patterns: HashMap<NodeIndex, crate::ErrorPatternPlan>,
    pub call_arguments: HashMap<NodeIndex, crate::CallArgumentPlan>,
    pub struct_constructions: HashMap<NodeIndex, crate::StructConstructionPlan>,
    pub enum_constructions: HashMap<NodeIndex, crate::EnumConstructionPlan>,
    pub enum_variants: HashMap<NodeIndex, crate::EnumVariantRef>,
    pub for_loops: HashMap<NodeIndex, crate::ForLoopPlan>,
    pub instance_methods: HashMap<NodeIndex, SymbolId>,
    pub application_calls: HashMap<NodeIndex, SymbolId>,
    pub update_calls: HashMap<NodeIndex, SymbolId>,
    pub concat_calls: HashMap<NodeIndex, SymbolId>,
}

impl DefaultBodyFacts {
    fn capture(r: &Resolver<'_>) -> Self {
        Self {
            nodes: HashSet::new(),
            node_symbols: r.node_symbols.clone(),
            constructor_types: r.constructor_types.clone(),
            enum_variant_indices: r.enum_variant_indices.clone(),
            node_field_indices: r.node_field_indices.clone(),
            node_type_values: r.node_type_values.clone(),
            node_coercions: r.node_coercions.clone(),
            error_constructions: r.error_constructions.clone(),
            error_conversions: r.error_conversions.clone(),
            error_propagations: r.error_propagations.clone(),
            error_eliminations: r.error_eliminations.clone(),
            error_patterns: r.error_patterns.clone(),
            call_arguments: r.call_arguments.clone(),
            struct_constructions: r.struct_constructions.clone(),
            enum_constructions: r.enum_constructions.clone(),
            enum_variants: r.enum_variants.clone(),
            for_loops: r.for_loops.clone(),
            instance_methods: r.instance_methods.clone(),
            application_calls: r.application_calls.clone(),
            update_calls: r.update_calls.clone(),
            concat_calls: r.concat_calls.clone(),
        }
    }
    fn for_body(r: &Resolver<'_>, nodes: &[NodeIndex]) -> Self {
        let mut facts = Self::capture(r);
        facts.nodes.extend(nodes.iter().copied());
        facts
            .node_symbols
            .retain(|node, _| facts.nodes.contains(node));
        facts
            .node_field_indices
            .retain(|node, _| facts.nodes.contains(node));
        facts
            .node_type_values
            .retain(|node, _| facts.nodes.contains(node));
        facts
            .node_coercions
            .retain(|node, _| facts.nodes.contains(node));
        facts
            .call_arguments
            .retain(|node, _| facts.nodes.contains(node));
        facts
            .constructor_types
            .retain(|node, _| facts.nodes.contains(node));
        facts
            .struct_constructions
            .retain(|node, _| facts.nodes.contains(node));
        facts
            .enum_constructions
            .retain(|node, _| facts.nodes.contains(node));
        facts
            .enum_variants
            .retain(|node, _| facts.nodes.contains(node));
        facts
            .instance_methods
            .retain(|node, _| facts.nodes.contains(node));
        facts
            .application_calls
            .retain(|node, _| facts.nodes.contains(node));
        facts
            .update_calls
            .retain(|node, _| facts.nodes.contains(node));
        facts
            .concat_calls
            .retain(|node, _| facts.nodes.contains(node));
        facts
            .error_constructions
            .retain(|node, _| facts.nodes.contains(node));
        facts
            .error_conversions
            .retain(|node, _| facts.nodes.contains(node));
        facts
            .error_propagations
            .retain(|node, _| facts.nodes.contains(node));
        facts
            .error_eliminations
            .retain(|node, _| facts.nodes.contains(node));
        facts
            .error_patterns
            .retain(|node, _| facts.nodes.contains(node));
        facts.for_loops.retain(|node, _| facts.nodes.contains(node));
        let symbols: HashSet<_> = facts.node_symbols.values().copied().collect();
        facts
            .enum_variant_indices
            .retain(|symbol, _| symbols.contains(symbol));
        facts
    }
    fn restore(&self, r: &mut Resolver<'_>) {
        r.node_symbols = self.node_symbols.clone();
        r.constructor_types = self.constructor_types.clone();
        r.enum_variant_indices = self.enum_variant_indices.clone();
        r.node_field_indices = self.node_field_indices.clone();
        r.node_type_values = self.node_type_values.clone();
        r.node_coercions = self.node_coercions.clone();
        r.error_constructions = self.error_constructions.clone();
        r.error_conversions = self.error_conversions.clone();
        r.error_propagations = self.error_propagations.clone();
        r.error_eliminations = self.error_eliminations.clone();
        r.error_patterns = self.error_patterns.clone();
        r.call_arguments = self.call_arguments.clone();
        r.struct_constructions = self.struct_constructions.clone();
        r.enum_constructions = self.enum_constructions.clone();
        r.enum_variants = self.enum_variants.clone();
        r.for_loops = self.for_loops.clone();
        r.instance_methods = self.instance_methods.clone();
        r.application_calls = self.application_calls.clone();
        r.update_calls = self.update_calls.clone();
        r.concat_calls = self.concat_calls.clone();
    }
}

pub(crate) struct CheckedBody {
    pub(crate) facts: DefaultBodyFacts,
    pub(crate) node_types: HashMap<NodeIndex, TypeIndex>,
    pub(crate) symbol_types: HashMap<SymbolId, TypeIndex>,
    pub(crate) coercion_targets: HashMap<NodeIndex, TypeIndex>,
    original_facts: DefaultBodyFacts,
    original_node_types: HashMap<NodeIndex, TypeIndex>,
    original_symbol_types: Vec<TypeIndex>,
    original_function_inference: crate::inference::InferenceState,
}
impl CheckedBody {
    pub(crate) fn restore(&self, r: &mut Resolver<'_>) {
        self.original_facts.restore(r);
        r.function_inference = self.original_function_inference.clone();
        r.node_types = self.original_node_types.clone();
        for (symbol, &ty) in r.symbols.iter_mut().zip(&self.original_symbol_types) {
            symbol.type_index = ty;
        }
    }
}

pub(crate) fn check_body(
    r: &mut Resolver<'_>,
    ast: &Ast,
    root: NodeIndex,
    implementor: TypeIndex,
    owner: TypeIndex,
    scope: Option<u32>,
    bindings: &[type_pool::AssociatedTypeBinding],
) -> Result<CheckedBody, String> {
    let original_facts = DefaultBodyFacts::capture(r);
    let original_function_inference = r.function_inference.clone();
    let original_node_types = r.node_types.clone();
    let original_symbol_types = r.symbols.iter().map(|symbol| symbol.type_index).collect();
    let provenance = crate::self_provenance::SelfProvenance::new(r, ast, root, owner)?;
    let mut pending = vec![root];
    let mut nodes = Vec::new();
    let mut self_value_paths = HashMap::new();
    while let Some(node) = pending.pop() {
        if node.is_null() {
            continue;
        }
        self_value_paths.insert(node, provenance.paths(r, ast, node, owner)?);
        nodes.push(node);
        pending.extend(ast.fixed_children(node));
        pending.extend(ast.multi_children(node));
    }
    for node in &nodes {
        r.function_inference.bodies.remove(node);
        r.function_inference.values.remove(node);
        r.function_inference.blocks.remove(node);
        if *node != root {
            r.function_inference.headers.remove(node);
        }
    }
    // A new specialization is a new fact context, not an update of the
    // previous one. In particular, a concrete value must not inherit an Any
    // assertion merely because that assertion existed in the shared template.
    let domain: HashSet<_> = nodes.iter().copied().collect();
    r.node_types.retain(|node, _| !domain.contains(node));
    r.node_type_values.retain(|node, _| !domain.contains(node));
    r.node_coercions.retain(|node, _| !domain.contains(node));
    r.error_constructions
        .retain(|node, _| !domain.contains(node));
    r.error_conversions.retain(|node, _| !domain.contains(node));
    r.error_propagations
        .retain(|node, _| !domain.contains(node));
    r.error_eliminations
        .retain(|node, _| !domain.contains(node));
    r.error_patterns.retain(|node, _| !domain.contains(node));
    r.node_field_indices
        .retain(|node, _| !domain.contains(node));
    r.call_arguments.retain(|node, _| !domain.contains(node));
    r.struct_constructions
        .retain(|node, _| !domain.contains(node));
    r.enum_constructions
        .retain(|node, _| !domain.contains(node));
    r.for_loops.retain(|node, _| !domain.contains(node));
    r.instance_methods.retain(|node, _| !domain.contains(node));
    r.application_calls.retain(|node, _| !domain.contains(node));
    r.update_calls.retain(|node, _| !domain.contains(node));
    r.concat_calls.retain(|node, _| !domain.contains(node));
    r.default_body_context = Some(DefaultBodyContext {
        implementor,
        trait_owner: owner,
        implementation_trait: owner,
        scope,
        bindings: bindings.to_vec(),
        self_value_paths,
    });
    crate::typing::prepare_replay_signatures(r, ast, ast.fixed_children(root)[2]);
    crate::typing::resolve_function_def_types(r, ast, root);
    r.default_body_context = None;
    let values_result = (|| -> Result<(), String> {
        for node in &nodes {
            if let Some(&ty) = r.node_type_values.get(node)
                && r.type_pool.contains_associated_type(ty)
            {
                let concrete = r
                    .type_pool
                    .specialize_associated_type(ty, bindings)
                    .map_err(|error| error.to_string())?;
                r.node_type_values.insert(*node, concrete);
            }
        }
        Ok(())
    })();
    if let Err(error) = values_result {
        original_facts.restore(r);
        r.function_inference = original_function_inference;
        r.node_types = original_node_types;
        for (symbol, ty) in r.symbols.iter_mut().zip(original_symbol_types) {
            symbol.type_index = ty;
        }
        return Err(error);
    }
    let node_types = nodes
        .iter()
        .filter_map(|node| r.node_types.get(node).map(|&ty| (*node, ty)))
        .collect();
    let mut symbol_types = HashMap::new();
    for node in &nodes {
        if let Some(&symbol) = r.node_symbols.get(node) {
            symbol_types.insert(symbol, r.symbols[symbol.0 as usize].type_index);
        }
    }
    let coercion_targets = nodes
        .iter()
        .filter_map(|node| {
            r.node_coercions
                .get(node)
                .map(|coercion| (*node, coercion.target))
        })
        .collect();
    Ok(CheckedBody {
        facts: DefaultBodyFacts::for_body(r, &nodes),
        node_types,
        symbol_types,
        coercion_targets,
        original_facts,
        original_function_inference,
        original_node_types,
        original_symbol_types,
    })
}
