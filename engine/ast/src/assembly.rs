//! Checked arena operations used when assembling separately parsed source units.
use std::fmt;

use crate::{Ast, NodeIndex, NodeKind};

/// Storage limits count the null sentinel and all retained child slots.
#[derive(Clone, Copy, Debug)]
pub struct AstStorageLimits {
    pub max_nodes: u32,
    pub max_children: u32,
}

impl Default for AstStorageLimits {
    fn default() -> Self {
        Self {
            max_nodes: u32::MAX,
            max_children: u32::MAX,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AstAssemblyError {
    MissingSentinel,
    InvalidSentinel,
    InvalidRoot(NodeIndex),
    InvalidChild { node: NodeIndex, child: NodeIndex },
    InvalidChildRange(NodeIndex),
    Cycle(NodeIndex),
    Capacity,
    Allocation,
}

impl fmt::Display for AstAssemblyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSentinel => f.write_str("AST has no null sentinel"),
            Self::InvalidSentinel => f.write_str("AST null sentinel is invalid"),
            Self::InvalidRoot(node) => write!(f, "AST root {} is out of bounds", node.0),
            Self::InvalidChild { node, child } => write!(
                f,
                "AST node {} references invalid child {}",
                node.0, child.0
            ),
            Self::InvalidChildRange(node) => {
                write!(f, "AST node {} has an invalid child range", node.0)
            }
            Self::Cycle(node) => write!(f, "AST child cycle reaches node {}", node.0),
            Self::Capacity => f.write_str("AST assembly exceeds storage limits"),
            Self::Allocation => f.write_str("cannot allocate AST assembly storage"),
        }
    }
}
impl std::error::Error for AstAssemblyError {}

impl Ast {
    /// Add a node referencing existing arena entries, checking index capacity and
    /// reserving storage before mutation. The supplied multi range is replaced
    /// by `multi`; the existing arena must already have valid storage.
    pub fn try_add_node(
        &mut self,
        mut node: crate::Node,
        multi: &[NodeIndex],
    ) -> Result<NodeIndex, AstAssemblyError> {
        let index = u32::try_from(self.nodes.len()).map_err(|_| AstAssemblyError::Capacity)?;
        if index == u32::MAX {
            return Err(AstAssemblyError::Capacity);
        }
        let end = self
            .extra_children
            .len()
            .checked_add(multi.len())
            .filter(|&end| end <= u32::MAX as usize)
            .ok_or(AstAssemblyError::Capacity)?;
        for &child in node.children.iter().chain(multi) {
            if child.0 as usize >= self.nodes.len() {
                return Err(AstAssemblyError::InvalidChild {
                    node: NodeIndex(index),
                    child,
                });
            }
        }
        self.nodes
            .try_reserve(1)
            .map_err(|_| AstAssemblyError::Allocation)?;
        self.extra_children
            .try_reserve(multi.len())
            .map_err(|_| AstAssemblyError::Allocation)?;
        self.original_spans
            .try_reserve((self.nodes.len() + 1).saturating_sub(self.original_spans.len()))
            .map_err(|_| AstAssemblyError::Allocation)?;
        node.multi_start = self.extra_children.len() as u32;
        node.multi_len = (end - self.extra_children.len()) as u32;
        self.extra_children.extend_from_slice(multi);
        Ok(self.push_node(node))
    }

    /// Check storage references and cycles without following unchecked indices.
    /// Orphan nodes and shared subtrees are permitted; null children are retained.
    pub fn validate_storage(&self) -> Result<(), AstAssemblyError> {
        let sentinel = self
            .nodes
            .first()
            .ok_or(AstAssemblyError::MissingSentinel)?;
        if sentinel.kind != NodeKind::Invalid
            || sentinel.children.iter().any(|child| !child.is_null())
            || sentinel.multi_len != 0
            || sentinel.multi_start != 0
        {
            return Err(AstAssemblyError::InvalidSentinel);
        }
        if self.nodes.len() > u32::MAX as usize || self.extra_children.len() > u32::MAX as usize {
            return Err(AstAssemblyError::Capacity);
        }
        if self.root.0 as usize >= self.nodes.len() {
            return Err(AstAssemblyError::InvalidRoot(self.root));
        }
        for (index, node) in self.nodes.iter().enumerate() {
            let index = NodeIndex(index as u32);
            let start = node.multi_start as usize;
            let end = start
                .checked_add(node.multi_len as usize)
                .ok_or(AstAssemblyError::InvalidChildRange(index))?;
            if end > self.extra_children.len() {
                return Err(AstAssemblyError::InvalidChildRange(index));
            }
            for &child in node.children.iter().chain(&self.extra_children[start..end]) {
                if child.0 as usize >= self.nodes.len() {
                    return Err(AstAssemblyError::InvalidChild { node: index, child });
                }
            }
        }
        // Even unused slots must be relocatable when the whole storage is moved.
        for &child in &self.extra_children {
            if child.0 as usize >= self.nodes.len() {
                return Err(AstAssemblyError::InvalidChild {
                    node: NodeIndex::NULL,
                    child,
                });
            }
        }
        let mut colors = Vec::new();
        colors
            .try_reserve_exact(self.nodes.len())
            .map_err(|_| AstAssemblyError::Allocation)?;
        colors.resize(self.nodes.len(), 0u8);
        let mut stack = Vec::new();
        stack
            .try_reserve(self.nodes.len())
            .map_err(|_| AstAssemblyError::Allocation)?;
        for root in 1..self.nodes.len() {
            if colors[root] != 0 {
                continue;
            }
            colors[root] = 1;
            stack.push((root, 0usize));
            while let Some((index, edge)) = stack.last_mut() {
                let node = &self.nodes[*index];
                let child = if *edge < 4 {
                    Some(node.children[*edge])
                } else if *edge - 4 < node.multi_len as usize {
                    Some(self.extra_children[node.multi_start as usize + *edge - 4])
                } else {
                    None
                };
                let Some(child) = child else {
                    colors[*index] = 2;
                    stack.pop();
                    continue;
                };
                *edge += 1;
                if child.is_null() {
                    continue;
                }
                let child_index = child.0 as usize;
                match colors[child_index] {
                    1 => return Err(AstAssemblyError::Cycle(child)),
                    0 => {
                        colors[child_index] = 1;
                        stack.push((child_index, 0));
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// Reject malformed or oversized arenas before changing the destination.
    /// Allocation capacity may grow on failure; logical contents remain unchanged.
    pub fn try_append_ast(&mut self, other: Ast) -> Result<NodeIndex, AstAssemblyError> {
        self.try_append_ast_with_limits(other, AstStorageLimits::default())
    }

    pub fn try_append_ast_with_limits(
        &mut self,
        other: Ast,
        limits: AstStorageLimits,
    ) -> Result<NodeIndex, AstAssemblyError> {
        let mut roots = self.try_append_asts_with_limits(vec![other], limits)?;
        Ok(roots.remove(0))
    }

    /// Preflight every unit once and append all units as one transaction.
    /// This avoids rescanning the growing destination for every package file.
    pub fn try_append_asts_with_limits(
        &mut self,
        others: Vec<Ast>,
        limits: AstStorageLimits,
    ) -> Result<Vec<NodeIndex>, AstAssemblyError> {
        let mut nodes = self.nodes.len();
        let mut children = self.extra_children.len();
        let mut sources = 0usize;
        for other in &others {
            let added = other
                .nodes
                .len()
                .checked_sub(1)
                .ok_or(AstAssemblyError::MissingSentinel)?;
            nodes = nodes.checked_add(added).ok_or(AstAssemblyError::Capacity)?;
            children = children
                .checked_add(other.extra_children.len())
                .ok_or(AstAssemblyError::Capacity)?;
            sources = sources
                .checked_add(other.source_records.len())
                .ok_or(AstAssemblyError::Capacity)?;
        }
        // Reject configured limits before allocating cycle-validation scratch.
        if nodes > limits.max_nodes as usize || children > limits.max_children as usize {
            return Err(AstAssemblyError::Capacity);
        }
        self.validate_storage()?;
        for other in &others {
            other.validate_storage()?;
        }
        let mut roots = Vec::new();
        roots
            .try_reserve_exact(others.len())
            .map_err(|_| AstAssemblyError::Allocation)?;
        self.nodes
            .try_reserve(nodes - self.nodes.len())
            .map_err(|_| AstAssemblyError::Allocation)?;
        self.extra_children
            .try_reserve(children - self.extra_children.len())
            .map_err(|_| AstAssemblyError::Allocation)?;
        self.original_spans
            .try_reserve(nodes.saturating_sub(self.original_spans.len()))
            .map_err(|_| AstAssemblyError::Allocation)?;
        self.source_records
            .try_reserve(sources)
            .map_err(|_| AstAssemblyError::Allocation)?;
        // Every arena is internally closed. Relocation cannot introduce cycles
        // between units, and all trusted appends fit the pre-reserved storage.
        for other in others {
            roots.push(self.append_ast(other));
        }
        Ok(roots)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Ast {
        let mut ast = Ast::new().with_source("42".into());
        let leaf = ast
            .builder(
                NodeKind::Int,
                rustc_span::Span::new(rustc_span::BytePos(0), rustc_span::BytePos(2)),
            )
            .build();
        ast.root = ast
            .builder(NodeKind::FileScope, rustc_span::DUMMY_SP)
            .add_multi_children(&[leaf, NodeIndex::NULL, leaf])
            .build();
        ast
    }

    #[test]
    fn rejects_storage_errors_and_cycles_before_mutation() {
        for kind in 0..6 {
            let mut bad = tree();
            match kind {
                0 => bad.nodes.clear(),
                1 => bad.nodes[0].kind = NodeKind::Int,
                2 => bad.root = NodeIndex(99),
                3 => bad.nodes[1].children[0] = NodeIndex(99),
                4 => bad.nodes[2].multi_len = u32::MAX,
                _ => bad.extra_children[0] = bad.root,
            }
            let mut destination = tree();
            let dump = crate::dump::dump_ast_to_string(&destination, destination.root);
            let nodes = destination.nodes.len();
            let children = destination.extra_children.clone();
            assert!(destination.try_append_ast(bad).is_err());
            assert_eq!(destination.nodes.len(), nodes);
            assert_eq!(destination.extra_children, children);
            assert_eq!(destination.source.as_deref(), Some("42"));
            assert_eq!(
                crate::dump::dump_ast_to_string(&destination, destination.root),
                dump
            );
        }
    }

    #[test]
    fn exact_limits_allow_relocation_and_one_below_rejects_atomically() {
        for (nodes, children) in [(4, 6), (5, 5)] {
            let mut ast = tree();
            assert_eq!(
                ast.try_append_ast_with_limits(
                    tree(),
                    AstStorageLimits {
                        max_nodes: nodes,
                        max_children: children
                    }
                ),
                Err(AstAssemblyError::Capacity)
            );
            assert_eq!((ast.nodes.len(), ast.extra_children.len()), (3, 3));
        }
        let mut ast = tree();
        let original_root = ast.root;
        let appended = ast
            .try_append_ast_with_limits(
                tree(),
                AstStorageLimits {
                    max_nodes: 5,
                    max_children: 6,
                },
            )
            .unwrap();
        assert_eq!(appended, NodeIndex(4));
        assert_eq!(ast.root, original_root);
        assert_eq!(
            ast.multi_children(appended),
            &[NodeIndex(3), NodeIndex::NULL, NodeIndex(3)]
        );
        assert_eq!(
            crate::dump::dump_ast_to_string(&ast, NodeIndex(3)),
            "(Int \"42\")"
        );
        ast.validate_storage().unwrap();
    }
    #[test]
    fn batch_rejects_late_invalid_unit_without_partial_append() {
        let mut ast = tree();
        let mut invalid = tree();
        invalid.extra_children.push(NodeIndex(99));
        assert!(
            ast.try_append_asts_with_limits(vec![tree(), invalid], AstStorageLimits::default())
                .is_err()
        );
        assert_eq!((ast.nodes.len(), ast.extra_children.len()), (3, 3));
        let roots = ast
            .try_append_asts_with_limits(
                vec![tree(), tree()],
                AstStorageLimits {
                    max_nodes: 7,
                    max_children: 9,
                },
            )
            .unwrap();
        assert_eq!(roots, [NodeIndex(4), NodeIndex(6)]);
        assert_eq!(
            ast.multi_children(roots[1]),
            [NodeIndex(5), NodeIndex::NULL, NodeIndex(5)]
        );
        ast.validate_storage().unwrap();
    }

    #[test]
    fn checked_node_creation_rejects_bad_references_without_mutating_storage() {
        let mut ast = tree();
        let node = crate::Node {
            kind: NodeKind::ModuleDef,
            span: rustc_span::DUMMY_SP,
            str_id: str_interner::StrId::from_raw(0),
            children: [
                NodeIndex(1),
                NodeIndex::NULL,
                NodeIndex::NULL,
                NodeIndex::NULL,
            ],
            multi_start: u32::MAX,
            multi_len: u32::MAX,
        };
        assert_eq!(
            ast.try_add_node(node.clone(), &[NodeIndex(99)]),
            Err(AstAssemblyError::InvalidChild {
                node: NodeIndex(3),
                child: NodeIndex(99)
            })
        );
        assert_eq!((ast.nodes.len(), ast.extra_children.len()), (3, 3));
        let added = ast
            .try_add_node(node, &[NodeIndex(2), NodeIndex::NULL])
            .unwrap();
        assert_eq!(added, NodeIndex(3));
        assert_eq!(ast.fixed_children(added)[0], NodeIndex(1));
        assert_eq!(ast.multi_children(added), [NodeIndex(2), NodeIndex::NULL]);
        ast.validate_storage().unwrap();
    }

    #[test]
    fn validation_handles_deep_trees_without_native_recursion() {
        let mut ast = Ast::new();
        let mut child = NodeIndex::NULL;
        for _ in 0..100_000 {
            child = ast
                .builder(NodeKind::Negative, rustc_span::DUMMY_SP)
                .add_child(child)
                .build();
        }
        ast.root = child;
        ast.validate_storage().unwrap();
        ast.nodes[1].children[0] = child;
        assert!(matches!(
            ast.validate_storage(),
            Err(AstAssemblyError::Cycle(_))
        ));
    }
}
