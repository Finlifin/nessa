# Package identity foundation API / schema 1

Public API (owned implementation):
- ManifestDocument::parse(&str) -> Result<ManifestDocument, PackageError>
- ManifestDocument::manifest(&self) -> &PackageManifest (legacy five fields unchanged)
- ManifestDocument::local_identity(&self) -> PackageIdentity
- ManifestDocument::metadata(&self) -> &toml::Table (complete original semantic TOML)
- PackageResolver::register_document(&mut self, ManifestDocument) -> Result<(), PackageError>
- PackageResolver::resolve_document(&self, &ManifestDocument) -> Result<ResolvedPackageGraph, PackageError>
- PackageResolver::resolve_document_locked(&self, &ManifestDocument, &str) -> Result<ResolvedPackageGraph, PackageError>
- ResolvedPackageGraph::root_identity(&self) -> PackageIdentity
- ResolvedPackageGraph::packages(&self) -> &[PackageManifest] (dependency order, includes root)
- ResolvedPackageGraph::identity(&self, qualified_name: &str) -> Option<PackageIdentity>
- ResolvedPackageGraph::to_lock(&self) -> String (infallible canonical TOML emission)
- PackageIdentity: Copy/Eq/Ord/Hash/Debug/Display lowercase 32 hex; as_bytes() -> &[u8;16].

The local identity is the same hash procedure with zero selected child edges. For a leaf it equals graph identity; for a package with dependencies it is local manifest information only. No concrete selected versions enter identity. Legacy resolver register/resolve remain available, use the same deterministic solver with synthetic documents (only legacy fields).

Canonical identity byte schema 1: SHA-256(input)[0..16], interpreted in byte order (big endian). Input starts literal bytes `nessa.package.identity\0`, then u32 BE schema 1, then canonical manifest semantic tree (remove ONLY `package.version` and move dependency constraints into a separate typed dependency list: string dependency entries are removed; inline dependency tables lose only `version`, retaining every other attribute; an empty remaining dependency table or empty top-level dependencies table is omitted), then u64 BE dependency count and each dependency sorted by qualified UTF-8 name: framed qualified name + canonical constraint ADT. Then u64 BE selected child count and each child sorted by qualified name: framed qualified name + raw16 child identity. A framed string is u64 BE byte length + UTF-8.

TOML tags: 1 string (framed); 2 signed integer (i64 BE); 3 float (IEEE754 u64 BE, all NaNs normalized 0x7ff8000000000000); 4 bool (byte 0/1); 5 datetime (date presence + year u16 BE/month/day bytes; time presence + hour/minute/second bytes + nanosecond u32 BE; offset presence + signed minute i32 BE, Z equals +00:00); 6 array (u64 count, ordered values); 7 table (u64 count, sorted framed UTF-8 keys + values). Distinct TOML types remain distinct; unknown metadata included.

Constraint ADT: u64 BE comparator count, comparators sorted/deduplicated by their encoded bytes. Each comparator: op byte (1 Exact,2 Greater,3 GreaterEq,4 Less,5 LessEq,6 Tilde,7 Caret,8 Wildcard), u64 BE major, optional minor (presence byte, then u64 BE), optional patch similarly, framed prerelease string. Canonical normalization applies ONLY when no comparator in the conjunction explicitly mentions a prerelease; otherwise all original optional-component presence is retained. For stable-only conjunctions: >= and < missing minor/patch become zero; ^ missing patch becomes zero when major>0 or minor>0, and missing minor becomes zero when major>0; ~ with specified minor gets missing patch zero. Strict > and <= partial bounds retain component presence, as do exact/wildcard partial bounds and ^0.0. Bare full version and =full version are exact, build metadata ignored for constraints. Whitespace/comparator order/duplicates are ignored. This is semantic comparator-ADT canonicalization, not arbitrary interval-equivalence minimization across different operators. Constraint matching ALWAYS uses the original semantic comparator request with semver prerelease gating and zero-major rules; hash-only normalization never alters matching.

Lock TOML exact schema (unknown keys rejected):
```
schema_version = 1
identity_schema = 1
[root]
qualified_name = "com.example/app"
version = "1.0.0"
identity = "32lowercasehex"
[[packages]]
qualified_name = "com.example/app"
version = "1.0.0"
identity = "32lowercasehex"
dependencies = ["com.example/child"]
```
Packages include root once; entries sorted qualified name, dependencies sorted qualified name. Each version is full exact semantic version including pre/build. Locked resolution ONLY uses pinned versions (never re-solves highest), checks every reachable edge constraint, cycles, root version/name, dependency adjacency and all recomputed identities. Extra/missing/duplicate/stale/damaged entries, unknown schema/keys reject. Root metadata content validation and child identities make arbitrary hash replacement fail. Registry registering inconsistent same qualified name + exact version is rejected (idempotent semantically identical registration allowed). Highest candidate ordering uses semantic version precedence, then full version spelling as deterministic build-metadata tie breaker; search is deterministic by sorted qualified names, with backtracking and joint constraints.

Resource bounds report PackageError::Capacity explicitly: manifest source16MiB, lock32MiB, 65536dependencies/package, 65536registered versions, registry input128MiB, 65536selected/locked packages, 1000000search steps/pending selected-map entries. Search shares immutable documents with Arc; no recursion/depth cap or raised process stack. These bound data/work, not tiny-depth semantic exceptions.
