# 稳定 TypeId 身份协议（版本 1）

这是本轮冻结的池 API、规范字节编码和 TPOL11 结构。生产实现与源码管线已合并，
全工作区检查和独立验收通过。最终状态见 [实施记录](stable-type-id-audit.md)。协议的 hash 输入
不包含池索引、驻留字符串编号和函数编号；归档中的局部引用仍使用索引。


Public records, exactly frozen contract fields:
PackageTypeContext { identity_schema:u32, identity:[u8;16], qualified_name:String, version:String }
IdentityPathSegment = Named(String) | Lexical { kind:u8, ordinal:u32 }
NominalTypeProvenance { type_index:TypeIndex, package:u32, path:Vec<IdentityPathSegment>,
last_stable_version:String }
TypeIdentityInput { schema:u32, packages:Vec<PackageTypeContext>,
declarations:Vec<NominalTypeProvenance> }
All records Clone/Debug/PartialEq/Eq. TypeIdentityError implements Error/Display.
TypePool::{finalize_type_identities(input)->Result<(),TypeIdentityError>,
stable_type_id(TypeIndex)->Result<TypeId,TypeIdentityError>,
identity_input()->Option<&TypeIdentityInput>,
validate_type_identities()->Result<(),TypeIdentityError>, is_reserved_type(TypeIndex)->bool}.
TypePoolSnapshot gains public identity_input:Option<TypeIdentityInput>. finalize is atomic,
validates every nominal provenance, abstracts and reserved/collisions; aliases receive exact
canonical target ID. Existing pool.validate/restore also recompute finalized identities. Lookup ZERO
is always None, reverse lookup aliases uses canonical target. Source provenance must skip
is_reserved_type indices; include nonreserved Struct/Enum/Newtype/Trait/Module and
NON-structurally-interned Effect declarations, including abstract nominal declarations (whose
runtime IDs stayZERO).

Reserved IDs: intrinsic existing high0x4e45535341494e00 and intrinsic ordinal;
List/Buffer/Map/MapBuffer existing high0x4e455353434f4c4c and low(1<<32)|1..4. New bootstrap traits
high0x4e45535354524954, low(1<<32)|ordinal1..8 in
Display/Hash/Eq/Ord/PartialEq/PartialOrd/Iterator/IntoIterator order. Legacy bootstrap values remain
legacy; never rewritten at decode. Computed IDs with ZERO or any of these reserved high words
reject.

All type-hash integers BE, strings and byte blobs framed u32 byte length then exact bytes, sequences
u32 count. IDs raw16 = BEhi + BElo. SHA256 first16 of literal domain bytes `nessa.type.identity\0` +
BEu32 schema1 + FRAMED root term + u32 reachable nominal record count + records sorted by anchor
bytes, each FRAMED anchor then FRAMED layout. No artifact-local indices, interner IDs, FuncIds,
access/scope IDs or own package-version/name string enter hash. Package identity and effective
stable version do. Reachable graph includes interface references and recursively referenced nominal
layouts, not unrelated declarations. Cyclic edges terminate at anchors. Transparent aliases never
create terms/records.

Anchor: nominal kindbyte (Struct1,Enum2,Newtype3,Trait4,Module5,Effect6), package raw16, FRAMED
last-stable-version, path u32 count; each segment Named byte0+FRAMED UTF8 or Lexical byte1+kind
u8+ordinal u32. Path includes original declaration name; no dot-join ambiguity.

Term tags: Reserved0+raw16; Nominal1+FRAMED anchor; Tuple2+size u32+align u32+ABI byte1+count and
FRAMED ordered element terms; Function3+size+align+ABI byte2+count/FRAMED ordered params+FRAMED
return; structural Effect4+size+align+ABI byte2+async bool byte+count/params+return;
Optional5+size+align+ABI byte3+FRAMED inner; ErrorQualified6 / EffectQualified7 +size+align+ABI
byte4+FRAMED inner+u32 setcount+FRAMED members sorted/deduplicated by term bytes. Symbolic
Associated8+FRAMED owner term+FRAMED name; IterationStepTemplate9+size+align+ABI byte5+FRAMED item;
concrete authenticated IterationStep10+size+align+ABI byte6+FRAMED Item. Symbolic terms may appear
in trait interfaces; abstract binders/templates and any unspecialized non-trait composite remain
runtime ZERO. Executable Trait views do not count as abstract.

Nominal layout: kindbyte +size u32+align u32+ABI same nominal kindbyte, followed by:
- Struct: fields u32 count; each FRAMED field name, FRAMED term, offset u32, has_default bool byte.
  ABI denotes 8-byte tagged slots.
- Enum: tagbits u8=25, headerbytes u8=8, variants u32 count; ordered FRAMED name+tag u32+field-list
  as Struct. ABI denotes immediate no-payload/tagged heap payload.
- Newtype: FRAMED inner term.
- Module: no more bytes (namespace only; descendants not implicitly included).
- Effect: async byte then ordered params/return as Function.
- Trait: ordered parents u32 count+FRAMED terms; associated declarations u32 count, each FRAMED
  name+defaulttype presence bool/FRAMED term if present+typed-default-expression presence
  bool/FRAMED expression if present; dispatch slots u32 count, each FRAMED owner term+FRAMED
  name+signature presence bool and if present FRAMED declaration term, Self paths count+FRAMED paths
  sorted/deduplicated, associated paths count+FRAMED records sorted/deduplicated (FRAMED owner
  term+FRAMED name+FRAMED path), parameter kinds count+byte codes Receiver0 Required1 Optional2
  ListVariadic3 MapVariadic4.
Trait path = count u32 +step bytes: Parameter0+u32, Return1, TupleElement2+u32, OptionalInner3,
ErrorInner4, ErrorMember5+u32 semantic member rank, EffectInner6, EffectMember7+u32 semantic member
rank, IterationItem8.
Typed default expression tags Required0, Concrete1+FRAMED term, Self2+FRAMED owner term,
Binding3+FRAMED owner term+FRAMED name, Optional4+FRAMED expr, IterationStep5+FRAMED expr,
Tuple6+count/FRAMED expr, Function7+count/FRAMED param expr+FRAMED return expr.

TPOL11: exact existing TPOL10 prefix/tables (LE encoding unchanged), then required identity-input
block: LEu32 schema; package list LEu32 count, each LEu32 identity_schema+raw16+UTF8 string(LEu32
byte length)+version string; declaration list LEu32 count, each LEu32 type_index+package index+path
list LEu32 count, each Named u8=0+UTF8 string or Lexical u8=1+kind u8+LEu32 ordinal; finally
last_stable_version string. Revision11 always requires input; legacy1..10 inputNone. Writer
selects11 iff finalized and refuses explicit downgrade. Reader restores exact indices/IDs,
recomputes all and rejects invalid
schemas/references/paths/version/layout/IDs/provenance/tamper/trailing/truncated/budget data. No
NSBC/NSAM outer change. Supplied package hashes are consistency inputs, not package-manifest
authentication.

Implementation resource/cost policy: 262144 descriptor/provenance/items, 256 path segments (existing
transparent/default expression limits retained), 64MiB per term/record/serialized identity block and
per-root hash input, 64MiB aggregate cached term bytes and record bytes, 512MiB cumulative canonical
hash input per finalization/validation. 513+ nominal edges are a positive test, not depth-capped.
Structural terms and nominal record encoding cached once; each root hashes its own finite reachable
graph (total canonical bytes necessarily scale with each reachable result). Immutable stable lookups
use cached IDs; descriptor/registration/interface changes invalidate an atomic validation flag.
Dirty lookup performs one successful full recomputation then returns to cached lookup; failures stay
dirty. Every explicit validate and restore recomputes. Public well_known redirection compared to
stored role seal.

Qualified signature paths walk the original declaration shape. ErrorMember/EffectMember ordinals are
converted to the member term's position in the encoded semantic-byte-sorted/deduplicated set;
Tuple/Parameter ordinals retain source order. This does not change the plainStruct golden. Finalized
abstract nominal metadata with ZERO is permitted as nonexecutable template state; legacy
nominal-binder rejection and NSBC executable contains_associated_type guards remain.

Nominal path final Named component must match descriptor declaration name (Effect has no descriptor
name). Total input string/path/package/declaration counts are checked before encoding allocation;
64MiB input bytes and262144 total input items. Package names reject whitespace/control/empty
segments.

## Source provenance API


`driver::CompilationIdentityContext` derives Clone/Debug:
- `pub package: type_pool::PackageTypeContext`
- `pub last_stable_version: Option<String>` (global USER-package override; None defaults to
  package.version; std separate)
- `pub type_versions: Vec<(Vec<type_pool::IdentityPathSegment>, String)>` (per original declaration
  path; Vec deliberately diagnoses duplicate paths rather than silently overwriting)

`CompilationIdentityContext::from_graph(&pkg_manager::ResolvedPackageGraph) -> Result<Self,String>`
uses validated graph root package (last in dependency order) and root_identity. No
dependencies/manifest discarded to derive that ID.
`Driver::compile_with_identity(&self, source:&str, context:CompilationIdentityContext) ->
CompileResult`.
`Driver::compile(source)` derives checked scratch manifest identity as below.

`resolution::SourcePackageIdentity` has the same three public fields as CompilationIdentityContext.
`ResolveOptions::package_identities: HashMap<ast::NodeIndex, SourcePackageIdentity>` keyed by true
AST package root (FileScope for user; unwrapped ModuleDef for std). Explicit caller entries must
identify roots in actual scope graph; unused roots and unused/duplicate/invalid version paths
diagnose. Each scope declaration gets original origin package.

`resolution::scratch_package_identity(source:&str) -> Result<type_pool::PackageTypeContext,String>`:
lex checked tokens; scratch-domain schema1 SHA256 first16 of normalized stream; embed digest into an
actual checked temporary ManifestDocument unknown metadata field `[package] source_hash`, alongside
`normalization_schema=1`, fixed `domain="org.nessa.scratch"`, `name="source"`, `version="0.0.0"`,
`type="tmp"`; then ManifestDocument::local_identity() supplies genuine package protocol schema1
Merkle identity. Token hash itself is NOT claimed to be a package manifest hash. Additional explicit
package roots in bare resolution without contexts derive scratch metadata salted by semantic root
path, not by AST ids/spans/access package numbers. std has a real library/std/package.toml separate
manifest context, unaffected by user versions/source.

Named package/type paths use original scope names; typed lexical segments use stable explicit kind
tags and normalized AST sibling occurrence order, never token/NodeKind discriminants, spans,
interner integers or process source counters. Path includes a root ModuleDef's name (e.g.
std.traits.Read); FileScope root contributes no name. Source aliases/import aliases do not rename
original declaration paths.

Implementation note: scope lexical tags are Block1, Lambda2, CaseArm3, ImplDef4, ImplTraitDef5,
ExtendDef6, ExtendTraitDef7, ForLoop8, WhileLoop9, BoolMatches10, HandlesStatement11,
PatternIfGuard12, PatternNot13. Bare resolution SourceMap fallback restores stripped BOM/CRLF via
checked normalization records; location counters remain absent from identity bytes. Empty source
FileScopes and empty modules remain true roots; context entries do not require child scopes.


源码来源由 `Ast.source: Option<String>` 表达：Parser 保存 `Some(原始源码)`，
包括已知空输入 `Some("")`；只有 `None` 才尝试受检的 SourceMap 重建。
缺失真实来源且无显式包上下文时给出诊断，不伪造临时包；空 SourceMap 的
诊断采用保留消息、标签、备注和帮助信息的纯文本回退。


完整 Error 接入新增实际 CatchArm 词法作用域时，使用追加稳定标签14。
原1..13含义和规范字节不变，IdentityPathSegment已有u8 kind编码可承载
此标签；这不修改身份hash schema或旧归档图。该标签实现与回归已随Error
阶段合入根工作区，并通过全仓与独立验收。身份编码规则及旧golden不变。
