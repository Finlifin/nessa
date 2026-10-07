//! Source-bound Self paths for default bodies; equal interface indices are not evidence.

use std::collections::{HashMap, HashSet};

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{TraitTypeStep, TypeIndex};

use crate::{SymbolId, resolver::Resolver};

type Paths = Vec<Vec<TraitTypeStep>>;

pub(crate) struct SelfProvenance {
    bindings: HashMap<SymbolId, NodeIndex>,
    writes: HashMap<SymbolId, Vec<NodeIndex>>,
    contextual_parameters: HashMap<NodeIndex, Paths>,
}

impl SelfProvenance {
    pub(crate) fn new(
        r: &Resolver<'_>,
        ast: &Ast,
        root: NodeIndex,
        owner: TypeIndex,
    ) -> Result<Self, String> {
        let mut result = Self {
            bindings: HashMap::new(),
            writes: HashMap::new(),
            contextual_parameters: HashMap::new(),
        };
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            if node.is_null() {
                continue;
            }
            let children = ast.fixed_children(node);
            let binding = match ast.node(node).kind {
                NodeKind::LetDecl
                | NodeKind::VarDecl
                | NodeKind::ConstDecl
                | NodeKind::ParamTyped
                | NodeKind::ParamLambda
                | NodeKind::ParamOptional => children.first().copied(),
                NodeKind::ParamSelf => Some(node),
                NodeKind::FunctionDef => children.first().copied(),
                _ => None,
            };
            if let Some(symbol) = binding.and_then(|binding| r.node_symbols.get(&binding)) {
                result.bindings.insert(*symbol, node);
            }
            if ast.node(node).kind == NodeKind::Assign
                && let Some(symbol) = r.node_symbols.get(&children[0])
            {
                result.writes.entry(*symbol).or_default().push(children[1]);
            }
            match ast.node(node).kind {
                NodeKind::FunctionDef | NodeKind::TraitDeriveFn => {
                    let paths =
                        crate::trait_signatures::annotation_self_paths(r, ast, children[1], owner)?;
                    result.contextualize(ast, children[2], &paths, 0)?;
                }
                NodeKind::Lambda => {
                    let paths =
                        crate::trait_signatures::annotation_self_paths(r, ast, children[1], owner)?;
                    result.contextualize(ast, children[0], &paths, 0)?;
                }
                NodeKind::LetDecl | NodeKind::VarDecl | NodeKind::ConstDecl => {
                    let paths =
                        crate::trait_signatures::annotation_self_paths(r, ast, children[1], owner)?;
                    result.contextualize(ast, children[2], &paths, 0)?;
                }
                _ => {}
            }
            pending.extend(children.iter().chain(ast.multi_children(node)).copied());
        }
        Ok(result)
    }

    fn contextualize(
        &mut self,
        ast: &Ast,
        node: NodeIndex,
        paths: &Paths,
        depth: usize,
    ) -> Result<(), String> {
        if node.is_null() || paths.is_empty() {
            return Ok(());
        }
        if depth >= 256 {
            return Err("default Self contextual provenance nesting is too deep".into());
        }
        match ast.node(node).kind {
            NodeKind::Lambda => {
                for (index, &parameter) in ast.multi_children(node).iter().enumerate() {
                    if ast
                        .fixed_children(parameter)
                        .get(1)
                        .is_none_or(|annotation| annotation.is_null())
                    {
                        let parameter_paths = paths
                            .iter()
                            .filter(|path| {
                                path.first() == Some(&TraitTypeStep::Parameter(index as u32))
                            })
                            .map(|path| path[1..].to_vec())
                            .collect();
                        self.contextual_parameters
                            .insert(parameter, parameter_paths);
                    }
                }
            }
            NodeKind::Block => {
                let mut returns = Vec::new();
                collect_returns(ast, node, &mut returns);
                if let Some(&last) = ast.multi_children(node).last() {
                    returns.push(last);
                }
                for value in returns {
                    self.contextualize(ast, value, paths, depth + 1)?;
                }
            }
            NodeKind::ExprStatement | NodeKind::ReturnStatement => {
                self.contextualize(ast, ast.fixed_children(node)[0], paths, depth + 1)?
            }
            NodeKind::IfStatement => {
                for &branch in &ast.fixed_children(node)[1..] {
                    self.contextualize(ast, branch, paths, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn paths(
        &self,
        r: &Resolver<'_>,
        ast: &Ast,
        node: NodeIndex,
        owner: TypeIndex,
    ) -> Result<Paths, String> {
        self.walk(r, ast, node, owner, &mut HashSet::new(), 0)
    }

    fn walk(
        &self,
        r: &Resolver<'_>,
        ast: &Ast,
        node: NodeIndex,
        owner: TypeIndex,
        visiting: &mut HashSet<NodeIndex>,
        depth: usize,
    ) -> Result<Paths, String> {
        if node.is_null() {
            return Ok(Vec::new());
        }
        if depth >= 256 {
            return Err("default Self value provenance nesting is too deep".into());
        }
        if !visiting.insert(node) {
            return Ok(Vec::new());
        }
        let children = ast.fixed_children(node);
        let mut recurse = |node| self.walk(r, ast, node, owner, visiting, depth + 1);
        let result = match ast.node(node).kind {
            NodeKind::ParamSelf => vec![vec![]],
            NodeKind::Id | NodeKind::SelfLower => {
                if let Some(symbol) = r.node_symbols.get(&node)
                    && let Some(binding) = self.bindings.get(symbol)
                {
                    let mut paths = recurse(*binding)?;
                    for &write in self.writes.get(symbol).into_iter().flatten() {
                        intersect(&mut paths, &recurse(write)?);
                    }
                    paths
                } else {
                    Vec::new()
                }
            }
            NodeKind::LetDecl | NodeKind::VarDecl | NodeKind::ConstDecl => {
                if !children[1].is_null() {
                    crate::trait_signatures::annotation_self_paths(r, ast, children[1], owner)?
                } else {
                    recurse(children[2])?
                }
            }
            NodeKind::ParamTyped | NodeKind::ParamLambda | NodeKind::ParamOptional => {
                if children[1].is_null() {
                    self.contextual_parameters
                        .get(&node)
                        .cloned()
                        .unwrap_or_default()
                } else {
                    crate::trait_signatures::annotation_self_paths(r, ast, children[1], owner)?
                }
            }
            NodeKind::TypeCast => {
                crate::trait_signatures::annotation_self_paths(r, ast, children[1], owner)?
            }
            NodeKind::ExprStatement | NodeKind::ReturnStatement => recurse(children[0])?,
            NodeKind::ErrorPropagation => recurse(children[0])?
                .into_iter()
                .filter_map(|path| {
                    matches!(path.first(), Some(TraitTypeStep::ErrorInner))
                        .then(|| path[1..].to_vec())
                })
                .collect(),
            NodeKind::ErrorConstruction => recurse(children[0])?
                .into_iter()
                .map(|mut path| {
                    path.insert(0, TraitTypeStep::ErrorMember(0));
                    path
                })
                .collect(),
            NodeKind::Block => {
                let mut returns = Vec::new();
                collect_returns(ast, node, &mut returns);
                if let Some(&last) = ast.multi_children(node).last() {
                    returns.push(last);
                }
                let mut values = returns.into_iter().map(&mut recurse);
                let mut paths = values.next().transpose()?.unwrap_or_default();
                for value in values {
                    intersect(&mut paths, &value?);
                }
                paths
            }
            NodeKind::IfStatement => {
                let mut paths = recurse(children[1])?;
                intersect(&mut paths, &recurse(children[2])?);
                paths
            }
            NodeKind::Tuple => {
                let mut paths = Vec::new();
                for (index, &element) in ast.multi_children(node).iter().enumerate() {
                    paths.extend(recurse(element)?.into_iter().map(|mut path| {
                        path.insert(0, TraitTypeStep::TupleElement(index as u32));
                        path
                    }));
                }
                paths
            }
            NodeKind::Projection => {
                let paths = recurse(children[0])?;
                let index = str_interner::get(ast.node(children[1]).str_id)
                    .parse::<u32>()
                    .ok();
                paths
                    .into_iter()
                    .filter_map(|path| match path.first() {
                        Some(TraitTypeStep::TupleElement(element)) if Some(*element) == index => {
                            Some(path[1..].to_vec())
                        }
                        _ => None,
                    })
                    .collect()
            }
            NodeKind::Lambda | NodeKind::FunctionDef => {
                let is_named = ast.node(node).kind == NodeKind::FunctionDef;
                let (body, annotation) = if is_named {
                    (children[2], children[1])
                } else {
                    (children[0], children[1])
                };
                let mut paths = Vec::new();
                for (index, &parameter) in ast.multi_children(node).iter().enumerate() {
                    paths.extend(recurse(parameter)?.into_iter().map(|mut path| {
                        path.insert(0, TraitTypeStep::Parameter(index as u32));
                        path
                    }));
                }
                let returns = if annotation.is_null() {
                    recurse(body)?
                } else {
                    crate::trait_signatures::annotation_self_paths(r, ast, annotation, owner)?
                };
                paths.extend(returns.into_iter().map(|mut path| {
                    path.insert(0, TraitTypeStep::Return);
                    path
                }));
                paths
            }
            NodeKind::Call => {
                let callee = children[0];
                if crate::type_factories::identity(r, callee).is_some() {
                    let [item] = ast.multi_children(node) else {
                        return Err("invalid IterationStep type argument count".into());
                    };
                    let paths = recurse(*item)?
                        .into_iter()
                        .map(|mut path| {
                            path.insert(0, TraitTypeStep::IterationItem);
                            path
                        })
                        .collect();
                    visiting.remove(&node);
                    return Ok(paths);
                }
                if ast.node(callee).kind == NodeKind::Projection
                    && let Some(symbol) = r.instance_methods.get(&callee)
                    && recurse(ast.fixed_children(callee)[0])?.contains(&Vec::new())
                {
                    let definition = &r.symbols[symbol.0 as usize];
                    let declaring_owner = r.scopes[definition.scope.0 as usize].assoc_type;
                    if let Some(declaring_owner) = declaring_owner {
                        crate::trait_signatures::signature(
                            r,
                            ast,
                            declaring_owner,
                            definition.def_node,
                        )?
                        .self_paths
                        .into_iter()
                        .filter_map(|path| {
                            (path.first() == Some(&TraitTypeStep::Return))
                                .then(|| path[1..].to_vec())
                        })
                        .collect()
                    } else {
                        Vec::new()
                    }
                } else {
                    recurse(callee)?
                        .into_iter()
                        .filter_map(|path| {
                            (path.first() == Some(&TraitTypeStep::Return))
                                .then(|| path[1..].to_vec())
                        })
                        .collect()
                }
            }
            _ => Vec::new(),
        };
        visiting.remove(&node);
        Ok(result)
    }
}

fn intersect(paths: &mut Paths, other: &Paths) {
    paths.retain(|path| other.contains(path));
}

fn collect_returns(ast: &Ast, node: NodeIndex, result: &mut Vec<NodeIndex>) {
    if node.is_null() {
        return;
    }
    match ast.node(node).kind {
        NodeKind::Lambda | NodeKind::FunctionDef => return,
        NodeKind::ReturnStatement => {
            result.push(node);
            return;
        }
        _ => {}
    }
    for &child in ast
        .fixed_children(node)
        .iter()
        .chain(ast.multi_children(node))
    {
        collect_returns(ast, child, result);
    }
}
