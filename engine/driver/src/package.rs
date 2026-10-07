//! Compile checked filesystem source trees through the ordinary source backend.
use std::collections::HashSet;
use std::path::Path;

use ast::{Ast, Node, NodeIndex, NodeKind};
use diagnostic::{Diagnostic, DiagnosticContext};
use pkg_manager::{ModuleSources, PackageSources, SourceFile, SourceLoadLimits};
use rustc_span::DUMMY_SP;

use crate::{CompileResult, Driver, RunResult, compilation_error};

pub(crate) struct SourceUnit<'a> {
    pub(crate) file: &'a SourceFile,
    pub(crate) path: &'a [String],
    parent: Option<usize>,
}

impl Driver {
    pub fn compile_package(&self, root: &Path) -> CompileResult {
        match pkg_manager::load_package_sources(root) {
            Ok(sources) => self.compile_package_sources(&sources),
            Err(error) => self.package_failure(error.to_string()),
        }
    }

    /// Compile owned sources without reopening their physical paths.
    pub fn compile_package_sources(&self, sources: &PackageSources) -> CompileResult {
        self.compile_package_sources_with_dependencies(sources, &[])
    }

    pub fn run_package(&self, root: &Path) -> RunResult {
        let compiled = self.compile_package(root);
        if compiled.has_errors {
            return RunResult::CompileError(compiled.diagnostics);
        }
        match compiled.into_artifact() {
            Ok(artifact) => self.run_artifact(artifact),
            Err(error) => RunResult::RuntimeError(error.to_string()),
        }
    }

    pub fn compile_package_to_archive(&self, root: &Path, output: &Path) -> std::io::Result<()> {
        let artifact = self.compile_package(root).into_artifact()?;
        std::fs::write(output, nsbc_io::write_artifact(&artifact)?)
    }

    pub fn emit_package_ast_dump(&self, root: &Path, output: &Path) -> std::io::Result<()> {
        let sources = pkg_manager::load_package_sources(root).map_err(std::io::Error::other)?;
        let (ast, _) = self
            .package_ast(&sources)
            .map_err(|errors| compilation_error(&errors))?;
        std::fs::write(output, ast::dump::dump_ast_to_string_pretty(&ast, ast.root))
    }

    pub(crate) fn package_failure(&self, message: String) -> CompileResult {
        CompileResult::failed(
            self.package_errors(message),
            type_pool::TypePool::with_intrinsics(),
        )
    }

    pub(crate) fn package_errors(&self, message: String) -> Vec<Diagnostic> {
        let context = DiagnosticContext::new(&self.source_map);
        context.error(message).emit(&context);
        context.diagnostics().to_vec()
    }

    pub(crate) fn package_ast(
        &self,
        sources: &PackageSources,
    ) -> Result<(Ast, Vec<Diagnostic>), Vec<Diagnostic>> {
        let units = source_units(sources).map_err(|error| self.package_errors(error))?;
        let mut asts = Vec::with_capacity(units.len());
        let mut diagnostics = Vec::new();
        for unit in &units {
            let (ast, warnings) =
                self.parse_source_named(&unit.file.source, &unit.file.file_path.to_string_lossy())?;
            diagnostics.extend(warnings);
            asts.push(ast);
        }
        let mut parsed = asts.into_iter();
        // A source tree always has its entry. Retain it as the primary raw source.
        let mut ast = parsed
            .next()
            .ok_or_else(|| self.package_errors("package has no entry".into()))?;
        let root = ast.root;
        let mut roots = vec![root];
        roots.extend(self.append_source_units(&mut ast, parsed.collect())?);
        let mut members: Vec<Vec<NodeIndex>> = roots
            .iter()
            .map(|&root| ast.multi_children(root).to_vec())
            .collect();
        let mut generated: Vec<Vec<NodeIndex>> = vec![Vec::new(); units.len()];
        for index in (1..units.len()).rev() {
            // Children are assembled in reverse preorder; reverse them back
            // when their parent's body becomes complete.
            members[index].extend(generated[index].iter().rev().copied());
            let parent = units[index]
                .parent
                .ok_or_else(|| self.package_errors("module has no parent".into()))?;
            let name = units[index]
                .path
                .last()
                .ok_or_else(|| self.package_errors("module has no name".into()))?;
            let matching: Vec<_> = members[parent]
                .iter()
                .copied()
                .filter(|&item| {
                    binding_name(&ast, item).is_some_and(|id| str_interner::get(id) == *name)
                })
                .collect();
            if matching.len() > 1 {
                return Err(self.package_errors(format!("duplicate module declaration `{name}`")));
            }
            let body = &members[index];
            if let Some(&item) = matching.first() {
                let declaration = unwrap_visibility(&ast, item);
                if ast.node(declaration).kind != NodeKind::ModuleDef {
                    return Err(self.package_errors(format!(
                        "filesystem module `{name}` conflicts with a source binding"
                    )));
                }
                if !ast.multi_children(declaration).is_empty() {
                    return Err(self.package_errors(format!(
                        "inline module `{name}` conflicts with filesystem module"
                    )));
                }
                let start = u32::try_from(ast.extra_children.len()).map_err(|_| {
                    self.package_errors("module child storage exceeds capacity".into())
                })?;
                let end = ast
                    .extra_children
                    .len()
                    .checked_add(body.len())
                    .filter(|&end| end <= u32::MAX as usize)
                    .ok_or_else(|| {
                        self.package_errors("module child storage exceeds capacity".into())
                    })?;
                ast.extra_children.try_reserve(body.len()).map_err(|_| {
                    self.package_errors("cannot allocate module child storage".into())
                })?;
                ast.extra_children.extend_from_slice(body);
                ast.nodes[declaration.0 as usize].multi_start = start;
                ast.nodes[declaration.0 as usize].multi_len = (end - start as usize) as u32;
            } else {
                let name_node = self.package_node(
                    &mut ast,
                    NodeKind::Id,
                    DUMMY_SP,
                    str_interner::intern(name),
                    NodeIndex::NULL,
                    &[],
                )?;
                let span = ast.node(roots[index]).span;
                let module = self.package_node(
                    &mut ast,
                    NodeKind::ModuleDef,
                    span,
                    str_interner::StrId::from_raw(0),
                    name_node,
                    body,
                )?;
                generated[parent].push(module);
            }
        }
        members[0].extend(generated[0].iter().rev().copied());
        let root_span = ast.node(roots[0]).span;
        ast.root = self.package_node(
            &mut ast,
            NodeKind::FileScope,
            root_span,
            str_interner::StrId::from_raw(0),
            NodeIndex::NULL,
            &members[0],
        )?;
        ast.validate_storage()
            .map_err(|error| self.package_errors(error.to_string()))?;
        Ok((ast, diagnostics))
    }
    pub(crate) fn package_node(
        &self,
        ast: &mut Ast,
        kind: NodeKind,
        span: rustc_span::Span,
        str_id: str_interner::StrId,
        name: NodeIndex,
        body: &[NodeIndex],
    ) -> Result<NodeIndex, Vec<Diagnostic>> {
        ast.try_add_node(
            Node {
                kind,
                span,
                str_id,
                children: [name, NodeIndex::NULL, NodeIndex::NULL, NodeIndex::NULL],
                multi_start: 0,
                multi_len: 0,
            },
            body,
        )
        .map_err(|error| self.package_errors(error.to_string()))
    }
}

fn unwrap_visibility(ast: &Ast, item: NodeIndex) -> NodeIndex {
    match ast.node(item).kind {
        NodeKind::PubDef | NodeKind::PrivateDef => ast.fixed_children(item)[0],
        _ => item,
    }
}

fn binding_name(ast: &Ast, item: NodeIndex) -> Option<str_interner::StrId> {
    let mut definition = unwrap_visibility(ast, item);
    if matches!(
        ast.node(definition).kind,
        NodeKind::GlobalDecl | NodeKind::AssocDecl
    ) {
        definition = ast.fixed_children(definition)[0];
    }
    if !matches!(
        ast.node(definition).kind,
        NodeKind::FunctionDef
            | NodeKind::ModuleDef
            | NodeKind::StructDef
            | NodeKind::EnumDef
            | NodeKind::TraitDef
            | NodeKind::EffectDef
            | NodeKind::AsyncEffectDef
            | NodeKind::Typealias
            | NodeKind::ConstDecl
            | NodeKind::LetDecl
            | NodeKind::VarDecl
            | NodeKind::AssocBinding
    ) {
        return None;
    }
    let name = ast.fixed_children(definition)[0];
    (ast.node(name).kind == NodeKind::Id).then_some(ast.node(name).str_id)
}

pub(crate) fn source_units(sources: &PackageSources) -> Result<Vec<SourceUnit<'_>>, String> {
    let limits = SourceLoadLimits::default();
    let mut units = vec![SourceUnit {
        file: &sources.entry,
        path: &[],
        parent: None,
    }];
    let mut bytes = sources.entry.source.len();
    if bytes > limits.max_file_bytes {
        return Err("package entry exceeds source byte limit".into());
    }
    let mut pending = sorted_children(&sources.modules, 0)?;
    while let Some((module, parent)) = pending.pop() {
        if units.len() >= limits.max_modules {
            return Err("package exceeds module count limit".into());
        }
        let path = &module.path;
        if path.len() >= limits.max_depth {
            return Err("package exceeds module depth limit".into());
        }
        if path.len() != units[parent].path.len() + 1 || !path.starts_with(units[parent].path) {
            return Err(format!("invalid module path hierarchy {:?}", path));
        }
        let name = path
            .last()
            .ok_or_else(|| "module path is empty".to_string())?;
        let (tokens, errors) = lexer::tokenize(name);
        if !errors.is_empty()
            || tokens.len() != 2
            || tokens[0].kind != lexer::TokenKind::Id
            || tokens[0].from != 0
            || tokens[0].to as usize != name.len()
        {
            return Err(format!("invalid module name `{name}`"));
        }
        if module.entry.source.len() > limits.max_file_bytes {
            return Err(format!("module `{name}` exceeds source byte limit"));
        }
        bytes = bytes
            .checked_add(module.entry.source.len())
            .filter(|&bytes| bytes <= limits.max_source_bytes)
            .ok_or("package exceeds total source byte limit")?;
        let index = units.len();
        units.push(SourceUnit {
            file: &module.entry,
            path,
            parent: Some(parent),
        });
        if units.len() + pending.len() + module.children.len() > limits.max_modules {
            return Err("package exceeds module count limit".into());
        }
        pending.extend(sorted_children(&module.children, index)?);
    }
    Ok(units)
}

fn sorted_children(
    children: &[ModuleSources],
    parent: usize,
) -> Result<Vec<(&ModuleSources, usize)>, String> {
    if children.len() > SourceLoadLimits::default().max_modules {
        return Err("package exceeds module count limit".into());
    }
    let mut names = HashSet::new();
    for child in children {
        if !names.insert(child.path.last()) {
            return Err("duplicate sibling module".into());
        }
    }
    let mut ordered: Vec<_> = children.iter().map(|child| (child, parent)).collect();
    ordered.sort_by(|a, b| b.0.path.cmp(&a.0.path));
    Ok(ordered)
}
