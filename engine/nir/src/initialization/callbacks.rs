//! Follow invoked callback arguments, rather than executing closure construction.

use std::collections::{BTreeSet, HashMap, HashSet};

use ast::{NodeIndex, NodeKind};
use resolution::{CallArgumentValue, ResolvedAst, SymbolId};

use super::{DefaultScan, Planner};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Pack {
    List,
    Map,
}

#[derive(Clone, Copy)]
struct Target<'a> {
    expression: NodeIndex,
    context: Option<DefaultScan<'a>>,
    pack: Option<Pack>,
}

type Bindings<'a> = HashMap<SymbolId, Target<'a>>;
type FlowKey = (
    NodeIndex,
    Vec<(u32, NodeIndex, Option<(u32, u32)>, Option<Pack>)>,
);

/// Only unwritten bindings have a stable callable source. Runtime global
/// guards remain responsible for arbitrary mutable function-pointer flows.
#[derive(Default)]
pub(super) struct Sources {
    aliases: HashMap<SymbolId, NodeIndex>,
    written: HashSet<SymbolId>,
}

pub(super) fn sources(resolved: &ResolvedAst) -> HashMap<Option<SymbolId>, Sources> {
    fn collect(resolved: &ResolvedAst, symbols: &HashMap<NodeIndex, SymbolId>) -> Sources {
        let ast = &resolved.ast;
        let mut result = Sources::default();
        for (index, data) in ast.nodes.iter().enumerate() {
            let node = NodeIndex(index as u32);
            let children = ast.fixed_children(node);
            match data.kind {
                NodeKind::LetDecl | NodeKind::ConstDecl
                    if ast.node(children[0]).kind == NodeKind::Id =>
                {
                    if let Some(&symbol) = symbols.get(&children[0]).or_else(|| symbols.get(&node))
                    {
                        result.aliases.insert(symbol, children[2]);
                    }
                }
                NodeKind::Assign
                | NodeKind::AddAssign
                | NodeKind::SubAssign
                | NodeKind::MulAssign
                | NodeKind::DivAssign
                | NodeKind::ModAssign => {
                    let mut target = children[0];
                    if ast.node(target).kind == NodeKind::Call {
                        target = ast.fixed_children(target)[0];
                    }
                    if ast.node(target).kind == NodeKind::Id
                        && let Some(&symbol) = symbols.get(&target)
                    {
                        result.written.insert(symbol);
                    }
                }
                NodeKind::Call => {
                    let callee = children[0];
                    if ast.node(callee).kind == NodeKind::Projection {
                        let projection = ast.fixed_children(callee);
                        if matches!(
                            str_interner::get(ast.node(projection[1]).str_id).as_str(),
                            "set" | "push" | "pop" | "remove" | "clear"
                        ) && let Some(&symbol) = symbols.get(&projection[0])
                        {
                            result.written.insert(symbol);
                        }
                    }
                }
                _ => {}
            }
        }
        let mut pending: Vec<_> = result.written.iter().copied().collect();
        while let Some(symbol) = pending.pop() {
            if let Some(&alias) = result.aliases.get(&symbol)
                && let Some(&source) = symbols.get(&alias)
                && result.written.insert(source)
            {
                pending.push(source);
            }
        }
        result
            .aliases
            .retain(|symbol, _| !result.written.contains(symbol));
        result
    }
    let mut result = HashMap::from([(None, collect(resolved, &resolved.node_symbols))]);
    for plan in &resolved.default_methods {
        if let Some(facts) = &plan.body_facts {
            result.insert(Some(plan.function), collect(resolved, &facts.node_symbols));
        }
    }
    result
}

impl Planner<'_> {
    pub(super) fn scan_callback_call(
        &self,
        call: NodeIndex,
        dependencies: &mut BTreeSet<usize>,
        visited: &mut HashSet<(SymbolId, Option<SymbolId>)>,
        context: Option<DefaultScan<'_>>,
    ) {
        self.follow_callback_call(
            call,
            dependencies,
            visited,
            context,
            &Bindings::new(),
            &mut HashSet::new(),
        );
    }

    fn callback_symbol(&self, target: Target<'_>) -> Option<SymbolId> {
        self.body_facts(target.context, target.expression)
            .map(|facts| &facts.node_symbols)
            .unwrap_or(&self.resolved.node_symbols)
            .get(&target.expression)
            .copied()
    }

    fn callback_target<'a>(&self, mut target: Target<'a>, bindings: &Bindings<'a>) -> Target<'a> {
        let mut seen = HashSet::new();
        loop {
            let ast = &self.resolved.ast;
            if target.pack.is_some() {
                return target;
            }
            if ast.node(target.expression).kind == NodeKind::ErrorPropagation {
                let operand = ast.fixed_children(target.expression)[0];
                if self.known_error_branch(operand, target.context)
                    != Some(resolution::ErrorPatternBranch::Error)
                {
                    target.expression = operand;
                    continue;
                }
            }
            if ast.node(target.expression).kind == NodeKind::TypeCast {
                target.expression = ast.fixed_children(target.expression)[0];
                continue;
            }
            if let Some(selected) = self.packed_selection(target, bindings) {
                target = selected;
                continue;
            }
            let Some(symbol) = self.callback_symbol(target) else {
                break;
            };
            if !seen.insert(symbol) {
                break;
            }
            let key = target
                .context
                .filter(|context| self.body_facts(Some(*context), target.expression).is_some())
                .map(|context| context.body.function);
            let Some(sources) = self.callback_sources.get(&key) else {
                break;
            };
            if sources.written.contains(&symbol) {
                break;
            }
            if let Some(&bound) = bindings.get(&symbol) {
                target = bound;
                continue;
            }
            if let Some(&expression) = sources.aliases.get(&symbol) {
                target.expression = expression;
            } else {
                break;
            }
        }
        target
    }

    /// Follow only a statically selected packed entry. Constructing another
    /// callback, including a discarded duplicate property, does not invoke it.
    fn packed_selection<'a>(
        &self,
        target: Target<'a>,
        bindings: &Bindings<'a>,
    ) -> Option<Target<'a>> {
        let ast = &self.resolved.ast;
        if ast.node(target.expression).kind != NodeKind::Call {
            return None;
        }
        let arguments = ast.multi_children(target.expression);
        if arguments.len() != 1 {
            return None;
        }
        let mut receiver = ast.fixed_children(target.expression)[0];
        if ast.node(receiver).kind == NodeKind::Projection {
            let children = ast.fixed_children(receiver);
            if str_interner::get(ast.node(children[1]).str_id) != "get" {
                return None;
            }
            receiver = children[0];
        }
        let source = self.callback_target(
            Target {
                expression: receiver,
                context: target.context,
                pack: None,
            },
            bindings,
        );
        let pack = source.pack?;
        let plans = self
            .body_facts(source.context, source.expression)
            .map(|facts| &facts.call_arguments)
            .unwrap_or(&self.resolved.call_arguments);
        let plan = plans.get(&source.expression)?;
        let selector = self.callback_target(
            Target {
                expression: arguments[0],
                context: target.context,
                pack: None,
            },
            bindings,
        );
        let selector = selector.expression;
        let index = match pack {
            Pack::List => {
                if ast.node(selector).kind != NodeKind::Int {
                    return None;
                }
                let index = usize::try_from(
                    ast::literal::integer_magnitude(&str_interner::get(ast.node(selector).str_id))
                        .ok()?,
                )
                .ok()?;
                plan.parameters
                    .iter()
                    .find_map(|binding| match &binding.value {
                        CallArgumentValue::Variadic { source_indices } => {
                            source_indices.get(index).copied()
                        }
                        _ => None,
                    })?
            }
            Pack::Map => {
                if ast.node(selector).kind != NodeKind::Str {
                    return None;
                }
                let raw = str_interner::get(ast.node(selector).str_id);
                let key = raw.strip_prefix('"')?.strip_suffix('"')?;
                // Escaped/dynamic keys and mutable provider flows keep their
                // runtime initialization guards instead of a guessed source.
                if key.contains('\\') {
                    return None;
                }
                let key = str_interner::intern(key);
                plan.parameters
                    .iter()
                    .find_map(|binding| match &binding.value {
                        CallArgumentValue::MapVariadic { source_properties } => source_properties
                            .iter()
                            .rev()
                            .find_map(|&(name, index)| (name == key).then_some(index)),
                        _ => None,
                    })?
            }
        };
        let expression = resolution::argument_value_node(
            ast,
            *ast.multi_children(source.expression).get(index)?,
        );
        Some(Target {
            expression,
            context: source.context,
            pack: None,
        })
    }

    fn scan_callback_target<'a>(
        &self,
        target: Target<'a>,
        dependencies: &mut BTreeSet<usize>,
        visited: &mut HashSet<(SymbolId, Option<SymbolId>)>,
        bindings: &Bindings<'a>,
        flow: &mut HashSet<FlowKey>,
    ) {
        let ast = &self.resolved.ast;
        if ast.node(target.expression).kind == NodeKind::Lambda {
            self.scan_initializer_in(
                ast.fixed_children(target.expression)[0],
                dependencies,
                visited,
                target.context,
            );
            self.follow_callback_body(
                ast.fixed_children(target.expression)[0],
                dependencies,
                visited,
                target.context,
                bindings,
                flow,
            );
        } else if let Some(symbol) = self.callback_symbol(target) {
            self.scan_called_symbol_in(symbol, dependencies, visited, target.context);
        }
    }

    fn follow_callback_call<'a>(
        &self,
        call: NodeIndex,
        dependencies: &mut BTreeSet<usize>,
        visited: &mut HashSet<(SymbolId, Option<SymbolId>)>,
        context: Option<DefaultScan<'a>>,
        outer: &Bindings<'a>,
        flow: &mut HashSet<FlowKey>,
    ) {
        let ast = &self.resolved.ast;
        let plans = self
            .body_facts(context, call)
            .map(|facts| &facts.call_arguments)
            .unwrap_or(&self.resolved.call_arguments);
        let Some(plan) = plans.get(&call) else { return };
        let body = match ast.node(plan.declaration).kind {
            NodeKind::FunctionDef => ast.fixed_children(plan.declaration)[2],
            NodeKind::Lambda => ast.fixed_children(plan.declaration)[0],
            _ => return,
        };
        let arguments = ast.multi_children(call);
        let mut bindings = outer.clone();
        for binding in &plan.parameters {
            let (expression, pack) = match binding.value {
                CallArgumentValue::Explicit { source_index } => {
                    let Some(&argument) = arguments.get(source_index) else {
                        continue;
                    };
                    (resolution::argument_value_node(ast, argument), None)
                }
                CallArgumentValue::Default { expression, .. } => (expression, None),
                CallArgumentValue::Variadic { .. } => (call, Some(Pack::List)),
                CallArgumentValue::MapVariadic { .. } => (call, Some(Pack::Map)),
            };
            let target = self.callback_target(
                Target {
                    expression,
                    context,
                    pack,
                },
                outer,
            );
            for &symbol in &binding.symbols {
                bindings.insert(symbol, target);
            }
        }
        let mut key: Vec<_> = bindings
            .iter()
            .map(|(symbol, target)| {
                (
                    symbol.0,
                    target.expression,
                    target
                        .context
                        .map(|context| (context.body.function.0, context.root.function.0)),
                    target.pack,
                )
            })
            .collect();
        key.sort_by_key(|entry| entry.0);
        if !flow.insert((plan.declaration, key)) {
            return;
        }
        self.follow_callback_body(body, dependencies, visited, context, &bindings, flow);
    }

    fn follow_callback_body<'a>(
        &self,
        root: NodeIndex,
        dependencies: &mut BTreeSet<usize>,
        visited: &mut HashSet<(SymbolId, Option<SymbolId>)>,
        context: Option<DefaultScan<'a>>,
        bindings: &Bindings<'a>,
        flow: &mut HashSet<FlowKey>,
    ) {
        let ast = &self.resolved.ast;
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            if node.is_null() {
                continue;
            }
            let children = ast.fixed_children(node);
            match ast.node(node).kind {
                NodeKind::Lambda | NodeKind::FunctionDef => continue,
                NodeKind::ErrorElimination => {
                    if let Some(arms) = self.selected_error_arms(node, context) {
                        pending.push(children[0]);
                        pending.extend(arms);
                        continue;
                    }
                }
                NodeKind::IfStatement => {
                    if let Some(condition) = self.static_boolean(children[0], &mut HashSet::new()) {
                        pending.push(children[0]);
                        pending.push(children[if condition { 1 } else { 2 }]);
                        continue;
                    }
                }
                NodeKind::BoolAnd | NodeKind::BoolOr => {
                    let left = self.static_boolean(children[0], &mut HashSet::new());
                    if matches!(
                        (ast.node(node).kind, left),
                        (NodeKind::BoolAnd, Some(false)) | (NodeKind::BoolOr, Some(true))
                    ) {
                        pending.push(children[0]);
                        continue;
                    }
                }
                NodeKind::WhileLoop
                    if self.static_boolean(children[1], &mut HashSet::new()) == Some(false) =>
                {
                    pending.push(children[1]);
                    continue;
                }
                NodeKind::Call | NodeKind::ExtendedCall => {
                    let callee = children[0];
                    let origin = Target {
                        expression: callee,
                        context,
                        pack: None,
                    };
                    let target = self.callback_target(origin, bindings);
                    if target.expression != callee {
                        self.scan_callback_target(target, dependencies, visited, bindings, flow);
                    }
                    self.follow_callback_call(node, dependencies, visited, context, bindings, flow);
                }
                _ => {}
            }
            pending.extend(children);
            pending.extend(ast.multi_children(node));
        }
    }
}
