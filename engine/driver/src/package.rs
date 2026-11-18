//! Compile checked filesystem source trees through the ordinary source backend.
use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use ast::{Ast, Node, NodeIndex, NodeKind};
use diagnostic::{Diagnostic, DiagnosticContext};
use pkg_manager::{ModuleSources, PackageSources, SourceFile, SourceLoadLimits};
use rustc_span::DUMMY_SP;

use crate::{CompileResult, Driver, RunResult, compilation_error};

pub(crate) struct SourceUnit<'a> {
    pub(crate) file: &'a SourceFile,
    pub(crate) path: &'a [String],
    pub(crate) parent: Option<usize>,
}

impl Driver {
    pub fn compile_package(&self, root: &Path) -> CompileResult {
        self.compile_package_with_lock_mode(root, false)
    }

    pub fn compile_package_locked(&self, root: &Path) -> CompileResult {
        self.compile_package_with_lock_mode(root, true)
    }

    fn compile_package_with_lock_mode(&self, root: &Path, require_lockfile: bool) -> CompileResult {
        match pkg_manager::load_package_workspace(root) {
            Ok(workspace) => {
                if let Err(message) = verify_workspace_lockfile(&workspace, require_lockfile) {
                    return self.package_failure(message);
                }
                self.compile_package_sources_with_dependencies(
                    &workspace.root,
                    &workspace.dependencies,
                )
            }
            Err(error) => self.package_failure(error.to_string()),
        }
    }

    /// Compile owned sources without reopening their physical paths.
    pub fn compile_package_sources(&self, sources: &PackageSources) -> CompileResult {
        self.compile_package_sources_with_dependencies(sources, &[])
    }

    pub fn compile_package_cached_artifact(
        &self,
        root: &Path,
    ) -> Result<nsbc::CompiledArtifact, Vec<Diagnostic>> {
        self.compile_package_cached_with_lock_mode(root, false)
    }

    pub fn compile_package_locked_cached_artifact(
        &self,
        root: &Path,
    ) -> Result<nsbc::CompiledArtifact, Vec<Diagnostic>> {
        self.compile_package_cached_with_lock_mode(root, true)
    }

    fn compile_package_cached_with_lock_mode(
        &self,
        root: &Path,
        require_lockfile: bool,
    ) -> Result<nsbc::CompiledArtifact, Vec<Diagnostic>> {
        let workspace = pkg_manager::load_package_workspace(root)
            .map_err(|error| self.package_errors(error.to_string()))?;
        if let Err(message) = verify_workspace_lockfile(&workspace, require_lockfile) {
            return Err(self.package_errors(message));
        }
        self.compile_package_sources_with_privileged_cached(
            &workspace.root,
            &workspace.dependencies,
            &BTreeSet::new(),
        )
    }

    pub fn run_package(&self, root: &Path) -> RunResult {
        self.run_package_with_lock_mode(root, false)
    }

    pub fn run_package_locked(&self, root: &Path) -> RunResult {
        self.run_package_with_lock_mode(root, true)
    }

    pub fn run_package_cached(&self, root: &Path) -> RunResult {
        match self.compile_package_cached_with_lock_mode(root, false) {
            Ok(artifact) => self.run_artifact(artifact),
            Err(diagnostics) => RunResult::CompileError(diagnostics),
        }
    }

    pub fn run_package_locked_cached(&self, root: &Path) -> RunResult {
        match self.compile_package_cached_with_lock_mode(root, true) {
            Ok(artifact) => self.run_artifact(artifact),
            Err(diagnostics) => RunResult::CompileError(diagnostics),
        }
    }

    fn run_package_with_lock_mode(&self, root: &Path, require_lockfile: bool) -> RunResult {
        let compiled = self.compile_package_with_lock_mode(root, require_lockfile);
        if compiled.has_errors {
            return RunResult::CompileError(compiled.diagnostics);
        }
        match compiled.into_artifact() {
            Ok(artifact) => self.run_artifact(artifact),
            Err(error) => RunResult::RuntimeError(error.to_string()),
        }
    }

    pub fn compile_package_to_archive(&self, root: &Path, output: &Path) -> std::io::Result<()> {
        let workspace = pkg_manager::load_package_workspace(root).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string())
        })?;
        let artifact = self
            .compile_package_sources_with_privileged_cached(
                &workspace.root,
                &workspace.dependencies,
                &BTreeSet::new(),
            )
            .map_err(|diagnostics| compilation_error(&diagnostics))?;
        let bytes = nsbc_io::write_artifact(&artifact)?;
        let has_local_deps = workspace
            .dependencies
            .iter()
            .any(|dep| !pkg_manager::is_bundled_package_root(&dep.root));
        if has_local_deps || workspace.existing_lockfile.is_some() {
            let lockfile = pkg_manager::Lockfile::from_workspace(&workspace);
            if workspace.existing_lockfile.as_ref() != Some(&lockfile) {
                lockfile.write(&workspace.root.root.join("package.lock"))?;
            }
        }
        std::fs::write(output, bytes)
    }

    /// Compile a disk package to a standalone `.wasm` binary (`wasm32-wasi` or `wasm32-freestanding`).
    pub fn compile_package_to_wasm(
        &self,
        root: &Path,
        output: &Path,
        options: crate::WasmCompileOptions,
    ) -> std::io::Result<()> {
        let workspace = pkg_manager::load_package_workspace(root).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string())
        })?;
        let artifact = self
            .compile_package_sources_with_privileged_cached(
                &workspace.root,
                &workspace.dependencies,
                &BTreeSet::new(),
            )
            .map_err(|diagnostics| compilation_error(&diagnostics))?;
        let bytes = wasm::compile_artifact_to_wasm(&artifact, &options).map_err(|err| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("wasm compilation failed: {err}"),
            )
        })?;
        let has_local_deps = workspace
            .dependencies
            .iter()
            .any(|dep| !pkg_manager::is_bundled_package_root(&dep.root));
        if has_local_deps || workspace.existing_lockfile.is_some() {
            let lockfile = pkg_manager::Lockfile::from_workspace(&workspace);
            if workspace.existing_lockfile.as_ref() != Some(&lockfile) {
                lockfile.write(&workspace.root.root.join("package.lock"))?;
            }
        }
        std::fs::write(output, bytes)
    }

    /// Compile a disk package to a standalone `.wasm` binary while strictly enforcing `package.lock`.
    pub fn compile_package_to_wasm_locked(
        &self,
        root: &Path,
        output: &Path,
        options: crate::WasmCompileOptions,
    ) -> std::io::Result<()> {
        let workspace = pkg_manager::load_package_workspace(root).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string())
        })?;
        verify_workspace_lockfile(&workspace, true)
            .map_err(|msg| std::io::Error::new(std::io::ErrorKind::InvalidData, msg))?;
        let artifact = self
            .compile_package_sources_with_privileged_cached(
                &workspace.root,
                &workspace.dependencies,
                &BTreeSet::new(),
            )
            .map_err(|diagnostics| compilation_error(&diagnostics))?;
        let bytes = wasm::compile_artifact_to_wasm(&artifact, &options).map_err(|err| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("wasm compilation failed: {err}"),
            )
        })?;
        std::fs::write(output, bytes)
    }

    /// Compile a disk package to an `.nsbc` archive while strictly enforcing `package.lock`
    /// without modifying or creating `package.lock`.
    pub fn compile_package_to_archive_locked(
        &self,
        root: &Path,
        output: &Path,
    ) -> std::io::Result<()> {
        let workspace = pkg_manager::load_package_workspace(root).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string())
        })?;
        verify_workspace_lockfile(&workspace, true)
            .map_err(|msg| std::io::Error::new(std::io::ErrorKind::InvalidData, msg))?;
        let artifact = self
            .compile_package_sources_with_privileged_cached(
                &workspace.root,
                &workspace.dependencies,
                &BTreeSet::new(),
            )
            .map_err(|diagnostics| compilation_error(&diagnostics))?;
        let bytes = nsbc_io::write_artifact(&artifact)?;
        std::fs::write(output, bytes)
    }

    /// Deterministically compute and write/update `package.lock` for a disk package.
    pub fn sync_package_lockfile(&self, root: &Path) -> std::io::Result<pkg_manager::Lockfile> {
        let workspace = pkg_manager::load_package_workspace(root).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string())
        })?;
        let lockfile = pkg_manager::Lockfile::from_workspace(&workspace);
        if workspace.existing_lockfile.as_ref() != Some(&lockfile) {
            lockfile.write(&workspace.root.root.join("package.lock"))?;
        }
        Ok(lockfile)
    }

    /// Verify an existing `package.lock` on disk against all resolved local dependencies.
    pub fn verify_package_lockfile(&self, root: &Path) -> std::io::Result<()> {
        let workspace = pkg_manager::load_package_workspace(root).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string())
        })?;
        let lockfile = workspace.existing_lockfile.as_ref().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("missing lockfile in {}", workspace.root.root.display()),
            )
        })?;
        lockfile.verify(&workspace).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
        })
    }

    /// Compile a disk package and render its human-readable NSBC disassembly.
    pub fn disassemble_package(&self, root: &Path) -> std::io::Result<String> {
        let compiled = self.compile_package(root);
        if compiled.has_errors {
            return Err(compilation_error(&compiled.diagnostics));
        }
        let artifact = compiled.into_artifact()?;
        Ok(nsbc_io::disassemble_artifact(&artifact))
    }

    /// Compile a disk package with `--locked` verification and render its NSBC disassembly.
    pub fn disassemble_package_locked(&self, root: &Path) -> std::io::Result<String> {
        let compiled = self.compile_package_locked(root);
        if compiled.has_errors {
            return Err(compilation_error(&compiled.diagnostics));
        }
        let artifact = compiled.into_artifact()?;
        Ok(nsbc_io::disassemble_artifact(&artifact))
    }

    /// Compile a disk package and write its human-readable NSBC disassembly to `output`.
    pub fn emit_package_bc_dump(&self, root: &Path, output: &Path) -> std::io::Result<()> {
        let dump = self.disassemble_package(root)?;
        std::fs::write(output, dump)
    }

    pub fn emit_package_ast_dump(&self, root: &Path, output: &Path) -> std::io::Result<()> {
        self.emit_package_ast_dump_with_lock_mode(root, output, false)
    }

    pub fn emit_package_ast_dump_locked(&self, root: &Path, output: &Path) -> std::io::Result<()> {
        self.emit_package_ast_dump_with_lock_mode(root, output, true)
    }

    fn emit_package_ast_dump_with_lock_mode(
        &self,
        root: &Path,
        output: &Path,
        require_lockfile: bool,
    ) -> std::io::Result<()> {
        let workspace = pkg_manager::load_package_workspace(root).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, error.to_string())
        })?;
        verify_workspace_lockfile(&workspace, require_lockfile)
            .map_err(|msg| std::io::Error::new(std::io::ErrorKind::InvalidData, msg))?;
        let (mut ast, _) = self
            .package_ast(&workspace.root)
            .map_err(|errors| compilation_error(&errors))?;
        if !workspace.dependencies.is_empty() {
            let mut dep_asts = Vec::with_capacity(workspace.dependencies.len());
            for dep in &workspace.dependencies {
                let (dep_ast, _) = self
                    .package_ast(dep)
                    .map_err(|errors| compilation_error(&errors))?;
                dep_asts.push(dep_ast);
            }
            let bodies = self
                .append_source_units(&mut ast, dep_asts)
                .map_err(|errors| compilation_error(&errors))?;
            let mut members = ast.multi_children(ast.root).to_vec();
            for (dep, body) in workspace.dependencies.iter().zip(bodies) {
                let dep_name = &dep.manifest.manifest().name;
                let name_node = self
                    .package_node(
                        &mut ast,
                        NodeKind::Id,
                        DUMMY_SP,
                        str_interner::intern(dep_name),
                        NodeIndex::NULL,
                        &[],
                    )
                    .map_err(|errors| compilation_error(&errors))?;
                let items = ast.multi_children(body).to_vec();
                let span = ast.node(body).span;
                let mod_node = self
                    .package_node(
                        &mut ast,
                        NodeKind::ModuleDef,
                        span,
                        str_interner::StrId::from_raw(0),
                        name_node,
                        &items,
                    )
                    .map_err(|errors| compilation_error(&errors))?;
                members.push(mod_node);
            }
            let root_span = ast.node(ast.root).span;
            ast.root = self
                .package_node(
                    &mut ast,
                    NodeKind::FileScope,
                    root_span,
                    str_interner::StrId::from_raw(0),
                    NodeIndex::NULL,
                    &members,
                )
                .map_err(|errors| compilation_error(&errors))?;
        }
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
        if !sources.manifest.ffi().wasm.is_empty() {
            let qpkg = sources.manifest.manifest().qualified_name();
            let mut synth = String::new();
            for spec in &sources.manifest.ffi().wasm {
                let conflict = members[0].iter().any(|&item| {
                    binding_name(&ast, item).is_some_and(|id| str_interner::get(id) == spec.name)
                });
                if conflict {
                    return Err(self.package_errors(format!(
                        "[[ffi.wasm]] module `{}` conflicts with an existing declaration",
                        spec.name
                    )));
                }
                synth.push_str(&format!(
                    "pub mod {} {{\n    use std.prelude.*\n",
                    spec.name
                ));
                for exp in &spec.exports {
                    let params_src = exp
                        .params
                        .iter()
                        .enumerate()
                        .map(|(i, ty)| format!("_p{i}: {ty}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let body_src = if exp.ret == "Unit" {
                        format!(
                            "let _ = std.runtime.backtrace(message = \"__nessa_wasm_bind:{qpkg}::{}::{}\"); ()",
                            spec.name, exp.nessa_name
                        )
                    } else {
                        format!(
                            "let _m: Any = std.runtime.backtrace(message = \"__nessa_wasm_bind:{qpkg}::{}::{}\"); _m.as({})",
                            spec.name, exp.nessa_name, exp.ret
                        )
                    };
                    synth.push_str(&format!(
                        "    pub fn {}({}) -> {} {{ {body_src} }}\n",
                        exp.nessa_name, params_src, exp.ret
                    ));
                }
                synth.push_str("}\n");
            }
            let (synth_ast, warnings) =
                self.parse_source_named(&synth, "ffi-wasm-generated-wrappers")?;
            diagnostics.extend(warnings);
            let appended = self.append_source_units(&mut ast, vec![synth_ast])?[0];
            members[0].extend_from_slice(ast.multi_children(appended));
        }
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

fn verify_workspace_lockfile(
    workspace: &pkg_manager::PackageWorkspace,
    require_lockfile: bool,
) -> Result<(), String> {
    let has_local_deps = workspace
        .dependencies
        .iter()
        .any(|dep| !pkg_manager::is_bundled_package_root(&dep.root));
    match &workspace.existing_lockfile {
        Some(lockfile) => lockfile.verify(workspace).map_err(|err| err.to_string()),
        None if require_lockfile && has_local_deps => Err(format!(
            "missing package.lock in {} (--locked requires an up-to-date lockfile)",
            workspace.root.root.display()
        )),
        None => Ok(()),
    }
}

pub(crate) fn unwrap_visibility(ast: &Ast, item: NodeIndex) -> NodeIndex {
    match ast.node(item).kind {
        NodeKind::PubDef | NodeKind::PrivateDef => ast.fixed_children(item)[0],
        _ => item,
    }
}

pub(crate) fn binding_name(ast: &Ast, item: NodeIndex) -> Option<str_interner::StrId> {
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
            | NodeKind::Newtype
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
