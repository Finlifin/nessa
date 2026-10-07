//! Original declaration provenance and checked identity publication.

use std::collections::{HashMap, HashSet};

use ast::{Ast, NodeKind};
use pkg_manager::Version;
use type_pool::{
    IdentityPathSegment, NominalTypeProvenance, PackageTypeContext, TypeIdentityInput,
};

use crate::resolver::Resolver;
use crate::{ScopeId, Symbol};

const MAX_IDENTITY_PATH: usize = 256;

/// Package identity and explicit type-version overrides for one source root.
#[derive(Debug, Clone)]
pub struct SourcePackageIdentity {
    pub package: PackageTypeContext,
    pub last_stable_version: Option<String>,
    /// Original typed paths, not import aliases; duplicate paths are diagnosed.
    pub type_versions: Vec<(Vec<IdentityPathSegment>, String)>,
}

pub(crate) fn finalize(r: &mut Resolver<'_>, ast: &Ast) {
    if let Err(message) = build_input(r, ast).and_then(|input| {
        r.type_pool
            .finalize_type_identities(input)
            .map_err(|error| error.to_string())
    }) {
        let span = ast.node(ast.root).span;
        let mut diagnostic = r
            .diag_ctx
            .error(format!("cannot finalize source type identities: {message}"));
        if !span.is_dummy()
            && r.diag_ctx.source_map().files().iter().any(|file| {
                file.start_pos <= span.lo() && span.lo() <= span.hi() && span.hi() <= file.end_pos
            })
        {
            diagnostic = diagnostic.with_primary_span(span);
        }
        diagnostic.emit(r.diag_ctx);
    }
}

fn build_input(r: &Resolver<'_>, ast: &Ast) -> Result<TypeIdentityInput, String> {
    let paths = scope_paths(r, ast)?;
    let mut contexts = r.options.package_identities.clone();
    let mut roots = HashSet::new();
    for scope in r.scopes.iter().skip(1) {
        let root = crate::imports::package_scope(r, scope.id);
        roots.insert(root);
    }
    let source = source_text(r, ast);
    for &root in &roots {
        let node = r.scopes[root.0 as usize].node;
        if contexts.contains_key(&node) {
            continue;
        }
        let source = source.as_deref().ok_or(
            "source contents are unavailable; provide an explicit package identity context",
        )?;
        let root_path = if node == ast.root {
            &[][..]
        } else {
            &paths[root.0 as usize]
        };
        contexts.insert(
            node,
            SourcePackageIdentity {
                package: crate::source_identity::scratch_for_path(source, root_path)?,
                last_stable_version: None,
                type_versions: Vec::new(),
            },
        );
    }
    for &node in contexts.keys() {
        if !roots
            .iter()
            .any(|root| r.scopes[root.0 as usize].node == node)
        {
            return Err("package identity context does not identify an actual package root".into());
        }
    }
    let mut root_order: Vec<_> = roots.into_iter().collect();
    root_order.sort_by(|left, right| {
        let left_node = r.scopes[left.0 as usize].node;
        let right_node = r.scopes[right.0 as usize].node;
        let left = &contexts[&left_node].package;
        let right = &contexts[&right_node].package;
        left.identity
            .cmp(&right.identity)
            .then(left.qualified_name.cmp(&right.qualified_name))
            .then(left.version.cmp(&right.version))
    });
    let mut packages = Vec::new();
    let mut root_packages = HashMap::new();
    let mut used = HashMap::new();
    for &root in &root_order {
        let node = r.scopes[root.0 as usize].node;
        let context = &contexts[&node];
        checked_version(&context.package.version)?;
        if let Some(version) = &context.last_stable_version {
            checked_version(version)?;
        }
        for (index, (path, version)) in context.type_versions.iter().enumerate() {
            if path.is_empty() || path.len() > MAX_IDENTITY_PATH {
                return Err(
                    "type version override requires a bounded nonempty declaration path".into(),
                );
            }
            if context.type_versions[..index]
                .iter()
                .any(|(previous, _)| previous == path)
            {
                return Err("duplicate type version override path".into());
            }
            checked_version(version)?;
        }
        let package = if let Some(index) = packages
            .iter()
            .position(|package| package == &context.package)
        {
            index
        } else {
            packages.push(context.package.clone());
            packages.len() - 1
        };
        root_packages.insert(
            root,
            u32::try_from(package).map_err(|_| "too many source packages")?,
        );
        used.insert(root, vec![false; context.type_versions.len()]);
    }
    let mut declarations = Vec::new();
    let mut types = HashSet::new();
    for symbol in &r.symbols {
        if symbol.def_node.is_null()
            || !matches!(
                ast.node(symbol.def_node).kind,
                NodeKind::StructDef
                    | NodeKind::EnumDef
                    | NodeKind::TraitDef
                    | NodeKind::ModuleDef
                    | NodeKind::EffectDef
                    | NodeKind::AsyncEffectDef
            )
            || r.type_pool.is_reserved_type(symbol.type_index)
            || !types.insert(symbol.type_index)
        {
            continue;
        }
        let (root, path) = declaration_path(r, ast, symbol, &paths)?;
        let context = &contexts[&r.scopes[root.0 as usize].node];
        let override_index = context
            .type_versions
            .iter()
            .position(|(candidate, _)| candidate == &path);
        let version = if let Some(index) = override_index {
            used.get_mut(&root)
                .ok_or("missing source package version state")?[index] = true;
            &context.type_versions[index].1
        } else {
            context
                .last_stable_version
                .as_ref()
                .unwrap_or(&context.package.version)
        };
        declarations.push(NominalTypeProvenance {
            type_index: symbol.type_index,
            package: root_packages[&root],
            path,
            last_stable_version: checked_version(version)?,
        });
    }
    if used
        .values()
        .any(|entries| entries.iter().any(|used| !used))
    {
        return Err(
            "type version override path does not identify an original nominal declaration".into(),
        );
    }
    Ok(TypeIdentityInput {
        schema: 1,
        packages,
        declarations,
    })
}

fn checked_version(version: &str) -> Result<String, String> {
    Version::parse(version)
        .map(|version| version.to_string())
        .ok_or_else(|| format!("invalid full semantic type version `{version}`"))
}

fn source_text(r: &Resolver<'_>, ast: &Ast) -> Option<String> {
    if let Some(source) = &ast.source {
        return Some(source.clone());
    }
    // Positions only locate the original contents; they never enter identity.
    // SourceMap separates even empty files by one address-space byte.
    let span = ast.node(ast.root).span;
    let file = r
        .diag_ctx
        .source_map()
        .files()
        .into_iter()
        .filter(|file| file.start_pos <= span.lo())
        .max_by_key(|file| file.start_pos)?;
    if span.hi() < span.lo()
        || span.hi() > file.end_pos
        || (span.lo() == span.hi() && !file.src.as_ref()?.is_empty())
    {
        // A hand-built dummy span cannot establish provenance for another file.
        return None;
    }
    source_file_text(&file)
}

fn source_file_text(file: &rustc_span::SourceFile) -> Option<String> {
    let source = file.src.as_ref()?;
    let removed = file
        .normalized_pos
        .last()
        .map_or(0, |position| position.diff);
    let length = source.len().checked_add(removed as usize)?;
    if length > 16 * 1024 * 1024 {
        return None;
    }
    // rustc_span strips BOM and CR from CRLF. Its cumulative removal records
    // retain enough information to restore literal and macro bytes exactly.
    let mut original = String::with_capacity(length);
    let mut copied = 0;
    let mut previous_diff = 0;
    for position in &file.normalized_pos {
        let offset = position.pos.0.checked_sub(file.start_pos.0)? as usize;
        let removed = position.diff.checked_sub(previous_diff)?;
        if offset == 0 && previous_diff == 0 && removed == 3 {
            original.push('\u{feff}');
        } else if removed == 1 {
            let newline = offset.checked_sub(1)?;
            if newline < copied || source.as_bytes().get(newline) != Some(&b'\n') {
                return None;
            }
            original.push_str(source.get(copied..newline)?);
            original.push('\r');
            copied = newline;
        } else {
            return None;
        }
        previous_diff = position.diff;
    }
    original.push_str(source.get(copied..)?);
    Some(original)
}

fn declaration_path(
    r: &Resolver<'_>,
    ast: &Ast,
    symbol: &Symbol,
    paths: &[Vec<IdentityPathSegment>],
) -> Result<(ScopeId, Vec<IdentityPathSegment>), String> {
    let root = if r.options.package_roots.contains(&symbol.def_node) {
        r.scopes
            .iter()
            .find(|scope| scope.node == symbol.def_node)
            .map(|scope| scope.id)
            .ok_or("source package root has no declaration scope")?
    } else {
        crate::imports::package_scope(r, symbol.scope)
    };
    let root_scope = &r.scopes[root.0 as usize];
    if r.options.detached_package_roots.contains(&root_scope.node)
        && symbol.def_node == root_scope.node
    {
        // This container is not a source declaration. A reserved, unlexable
        // segment keeps it distinct from a real module named after the package.
        return bounded_path(
            root,
            vec![
                IdentityPathSegment::Named("<package-root>".into()),
                IdentityPathSegment::Named(str_interner::get(symbol.name)),
            ],
        );
    }
    let prefix = root_scope
        .parent
        .map(|parent| paths[parent.0 as usize].len())
        .unwrap_or(0);
    let mut path = paths[symbol.scope.0 as usize].clone();
    path.push(IdentityPathSegment::Named(str_interner::get(symbol.name)));
    // A package root declaration belongs to its own package, not its parent.
    if ast.node(root_scope.node).kind == NodeKind::FileScope {
        return bounded_path(root, path);
    }
    let prefix = if r.options.detached_package_roots.contains(&root_scope.node)
        && symbol.def_node != root_scope.node
    {
        paths[root.0 as usize].len()
    } else {
        prefix
    };
    if prefix > path.len() {
        return Err("source declaration is outside its package root".into());
    }
    path.drain(..prefix);
    bounded_path(root, path)
}

fn bounded_path(
    root: ScopeId,
    path: Vec<IdentityPathSegment>,
) -> Result<(ScopeId, Vec<IdentityPathSegment>), String> {
    if path.is_empty() || path.len() > MAX_IDENTITY_PATH {
        return Err("source declaration identity path exceeds 256 segments".into());
    }
    Ok((root, path))
}

fn scope_paths(r: &Resolver<'_>, ast: &Ast) -> Result<Vec<Vec<IdentityPathSegment>>, String> {
    let mut ranks = HashMap::new();
    let mut pending = vec![ast.root];
    while let Some(node) = pending.pop() {
        if node.is_null() || ranks.contains_key(&node) {
            continue;
        }
        ranks.insert(node, ranks.len());
        pending.extend(ast.multi_children(node).iter().rev().copied());
        pending.extend(ast.fixed_children(node).iter().rev().copied());
    }
    let mut segments = HashMap::new();
    let mut children: Vec<_> = r.scopes.iter().skip(1).collect();
    children.sort_by_key(|scope| ranks.get(&scope.node).copied().unwrap_or(usize::MAX));
    let mut ordinals = HashMap::<(Option<ScopeId>, u8), u32>::new();
    for scope in children {
        let kind = ast.node(scope.node).kind;
        if kind == NodeKind::FileScope || r.options.detached_package_roots.contains(&scope.node) {
            continue;
        }
        let segment = if matches!(
            kind,
            NodeKind::ModuleDef
                | NodeKind::StructDef
                | NodeKind::EnumDef
                | NodeKind::TraitDef
                | NodeKind::FunctionDef
                | NodeKind::TraitDefFn
                | NodeKind::TraitDeriveFn
                | NodeKind::EffectDef
                | NodeKind::AsyncEffectDef
                | NodeKind::EnumVariant
        ) {
            let name = ast.fixed_children(scope.node)[0];
            IdentityPathSegment::Named(str_interner::get(ast.node(name).str_id))
        } else {
            let kind = lexical_kind(kind)?;
            let ordinal = ordinals.entry((scope.parent, kind)).or_default();
            let segment = IdentityPathSegment::Lexical {
                kind,
                ordinal: *ordinal,
            };
            *ordinal = ordinal
                .checked_add(1)
                .ok_or("too many lexical identity scopes")?;
            segment
        };
        segments.insert(scope.id, segment);
    }
    let mut paths: Vec<Option<Vec<IdentityPathSegment>>> = vec![None; r.scopes.len()];
    paths[ScopeId::ROOT.0 as usize] = Some(Vec::new());
    for start in 1..r.scopes.len() {
        let mut chain = Vec::new();
        let mut current = ScopeId(start as u32);
        while paths
            .get(current.0 as usize)
            .and_then(Option::as_ref)
            .is_none()
        {
            if chain.len() >= MAX_IDENTITY_PATH {
                return Err("source lexical identity exceeds 256 scopes".into());
            }
            chain.push(current);
            current = r
                .scopes
                .get(current.0 as usize)
                .and_then(|scope| scope.parent)
                .ok_or("source scope has no valid parent")?;
        }
        let mut path = paths[current.0 as usize]
            .clone()
            .ok_or("missing source scope path")?;
        for scope in chain.into_iter().rev() {
            if let Some(segment) = segments.get(&scope) {
                path.push(segment.clone());
            }
            if path.len() > MAX_IDENTITY_PATH {
                return Err("source lexical identity exceeds 256 segments".into());
            }
            paths[scope.0 as usize] = Some(path.clone());
        }
    }
    paths
        .into_iter()
        .map(|path| path.ok_or("missing source identity path".into()))
        .collect()
}

// Schema-1 lexical tags are explicit stable labels, never NodeKind discriminants.
fn lexical_kind(kind: NodeKind) -> Result<u8, String> {
    match kind {
        NodeKind::Block => Ok(1),
        NodeKind::Lambda => Ok(2),
        NodeKind::CaseArm => Ok(3),
        NodeKind::ImplDef => Ok(4),
        NodeKind::ImplTraitDef => Ok(5),
        NodeKind::ExtendDef => Ok(6),
        NodeKind::ExtendTraitDef => Ok(7),
        NodeKind::ForLoop => Ok(8),
        NodeKind::WhileLoop => Ok(9),
        NodeKind::BoolMatches => Ok(10),
        NodeKind::HandlesStatement => Ok(11),
        NodeKind::PatternIfGuard => Ok(12),
        NodeKind::PatternNot => Ok(13),
        NodeKind::CatchArm => Ok(14),
        _ => Err("unsupported source lexical scope in type identity path".into()),
    }
}

#[cfg(test)]
mod tests {
    use diagnostic::DiagnosticContext;
    use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};

    use super::*;

    #[test]
    fn bare_parser_resolution_uses_actual_source_contents_for_provenance() {
        let source = "mod outer{struct Shape{value:i64}}";
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty());
        let map = SourceMap::new(FilePathMapping::empty());
        // Unrelated source entries must not change the selected source identity.
        map.new_source_file(
            FileName::Custom("unrelated.ns".into()),
            "let other=2".into(),
        );
        let file = map.new_source_file(FileName::Custom("arbitrary.ns".into()), source.into());
        let diagnostics = DiagnosticContext::new(&map);
        let mut ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        assert_eq!(ast.source.as_deref(), Some(source));
        ast.source = None; // Explicitly exercise the legacy SourceMap fallback.

        let resolved = crate::resolve(ast, &diagnostics);
        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
        let input = resolved.type_pool.identity_input().unwrap();
        assert_eq!(
            input.packages,
            vec![crate::scratch_package_identity(source).unwrap()]
        );
        let shape = input
            .declarations
            .iter()
            .find(|declaration| {
                declaration.path
                    == vec![
                        IdentityPathSegment::Named("outer".into()),
                        IdentityPathSegment::Named("Shape".into()),
                    ]
            })
            .unwrap();
        assert_eq!(shape.last_stable_version, "0.0.0");
        assert_ne!(
            resolved.type_pool.stable_type_id(shape.type_index).unwrap(),
            type_pool::TypeId::ZERO
        );
    }

    #[test]
    fn bare_parser_without_source_map_entries_finalizes_real_source_identity() {
        let source = "struct Shape{value:i64};fn main(){Shape{value:42}.value}";
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty());
        let map = SourceMap::new(FilePathMapping::empty());
        let diagnostics = DiagnosticContext::new(&map);
        let ast =
            parser::Parser::new(&tokens, source, &diagnostics, rustc_span::BytePos(0)).parse();
        assert_eq!(ast.source.as_deref(), Some(source));
        let resolved = crate::resolve(ast, &diagnostics);
        assert!(!diagnostics.has_errors());
        let input = resolved.type_pool.identity_input().unwrap();
        assert_eq!(
            input.packages,
            vec![crate::scratch_package_identity(source).unwrap()]
        );
        let shape = input
            .declarations
            .iter()
            .find(|declaration| {
                declaration.path == vec![IdentityPathSegment::Named("Shape".into())]
            })
            .unwrap();
        assert_ne!(
            resolved.type_pool.stable_type_id(shape.type_index).unwrap(),
            type_pool::TypeId::ZERO
        );
    }

    #[test]
    fn bare_empty_parser_source_with_no_source_map_is_known_empty_provenance() {
        let source = "";
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty());
        let map = SourceMap::new(FilePathMapping::empty());
        let diagnostics = DiagnosticContext::new(&map);
        let ast =
            parser::Parser::new(&tokens, source, &diagnostics, rustc_span::BytePos(0)).parse();
        assert_eq!(ast.source.as_deref(), Some(""));
        let resolved = crate::resolve(ast, &diagnostics);
        assert!(!diagnostics.has_errors());
        assert_eq!(
            resolved.type_pool.identity_input().unwrap().packages,
            vec![crate::scratch_package_identity("").unwrap()]
        );
    }

    #[test]
    fn missing_handbuilt_source_is_a_spanless_error_with_an_empty_source_map() {
        let source = "struct Shape{value:i64}";
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty());
        let map = SourceMap::new(FilePathMapping::empty());
        let diagnostics = DiagnosticContext::new(&map);
        let mut ast =
            parser::Parser::new(&tokens, source, &diagnostics, rustc_span::BytePos(0)).parse();
        ast.source = None;
        let resolved = crate::resolve(ast, &diagnostics);
        assert!(diagnostics.has_errors());
        assert_eq!(diagnostics.error_count(), 1);
        assert!(resolved.type_pool.identity_input().is_none());
        let errors = diagnostics.diagnostics();
        assert_eq!(errors[0].primary_span, None);
        assert!(
            errors[0]
                .message
                .contains("source contents are unavailable")
        );
    }

    #[test]
    fn dummy_span_does_not_claim_unrelated_source_contents() {
        let source = "struct Shape{value:i64}";
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty());
        let map = SourceMap::new(FilePathMapping::empty());
        map.new_source_file(
            FileName::Custom("unrelated.ns".into()),
            "let unrelated=42".into(),
        );
        let diagnostics = DiagnosticContext::new(&map);
        // Parse without registered provenance, then resolve against an unrelated
        // map. A parser must reject mismatched registered bytes before resolution.
        let parse_map = SourceMap::new(FilePathMapping::empty());
        let parse_diagnostics = DiagnosticContext::new(&parse_map);
        let mut ast =
            parser::Parser::new(&tokens, source, &parse_diagnostics, rustc_span::BytePos(0))
                .parse();
        assert!(!parse_diagnostics.has_errors());
        ast.source = None;
        ast.nodes[ast.root.0 as usize].span = rustc_span::DUMMY_SP;
        let resolved = crate::resolve(ast, &diagnostics);
        assert!(diagnostics.has_errors());
        assert!(resolved.type_pool.identity_input().is_none());
        assert!(diagnostics.diagnostics().iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("source contents are unavailable")
        }));
    }

    #[test]
    fn existing_not_pattern_scopes_can_be_finalized() {
        let source = "fn main(){42 match {not 0=>42,_=>0}}";
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty());
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(FileName::Custom("patterns.ns".into()), source.into());
        let diagnostics = DiagnosticContext::new(&map);
        let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        let resolved = crate::resolve(ast, &diagnostics);
        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
        resolved.type_pool.validate_type_identities().unwrap();
    }

    #[test]
    fn source_map_reconstruction_preserves_original_literal_bytes() {
        let map = SourceMap::new(FilePathMapping::empty());
        for (index, original) in [
            "let text=\"first\r\nsecond\"\r\n",
            "\u{feff}let text=\"first\r\nsecond\"\r\n",
            "\r\n\r\n",
        ]
        .into_iter()
        .enumerate()
        {
            let file =
                map.new_source_file(FileName::Custom(format!("source-{index}")), original.into());
            assert_eq!(source_file_text(&file).as_deref(), Some(original));
        }
    }

    #[test]
    fn empty_source_and_empty_module_have_package_roots() {
        for (index, source) in ["", "mod empty{}"].into_iter().enumerate() {
            let (tokens, errors) = lexer::tokenize(source);
            assert!(errors.is_empty());
            let map = SourceMap::new(FilePathMapping::empty());
            map.new_source_file(FileName::Custom("before.ns".into()), "let before=1".into());
            let file =
                map.new_source_file(FileName::Custom(format!("empty-{index}")), source.into());
            let diagnostics = DiagnosticContext::new(&map);
            let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
            let package = crate::scratch_package_identity(source).unwrap();
            let mut options = crate::ResolveOptions::default();
            options.package_identities.insert(
                ast.root,
                SourcePackageIdentity {
                    package: package.clone(),
                    last_stable_version: None,
                    type_versions: Vec::new(),
                },
            );
            let resolved = crate::resolve_with_options(ast, &diagnostics, options);
            assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
            assert_eq!(
                resolved.type_pool.identity_input().unwrap().packages,
                vec![package]
            );
        }
    }

    #[test]
    fn stable_versions_require_full_semver() {
        assert_eq!(
            checked_version("2.3.4-beta.1+build.5").unwrap(),
            "2.3.4-beta.1+build.5"
        );
        for invalid in ["2.3", "02.3.4", "", "latest"] {
            assert!(checked_version(invalid).is_err());
        }
    }
}
