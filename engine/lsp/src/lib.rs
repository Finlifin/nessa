//! `nessa-lsp` — Language Server Protocol (LSP 3.17) implementation for Nessa.
//!
//! Combines Nessa's fault-tolerant lexer, parser, multi-package resolver (`driver::Driver::check_*_ide`),
//! live in-memory VFS overlays, and `last_good` semantic snapshot caching to provide responsive
//! diagnostics, hover, go-to-definition (including `std` and bundled `@regex`), type definition,
//! find implementations, find references, document highlights, rename, completions, signature help,
//! document/workspace symbols, semantic tokens, folding ranges, and pluggable formatting.

pub mod analysis;
pub mod cli;
pub mod index;
pub mod json;
pub mod position;
pub mod rpc;
pub mod server;

pub use analysis::{
    DocumentState, FormatDocFn, FormatRangeFn, LspDiagnostic, PackageSnapshot, RelatedDiagInfo,
    ServerConfig, WorkspaceAnalyzer, default_format_doc, default_format_range,
};
pub use cli::{help_text, run_cli, run_cli_with_config};
pub use index::{
    MemberEntry, Occurrence, ParamDoc, SemanticIndex, SymbolInfo, SymbolKey, TraitImplEntry,
};
pub use json::{JsonParseError, JsonValue};
pub use position::{LineIndex, LspPosition, LspRange, path_to_uri, uri_to_path};
pub use rpc::{RequestId, RpcError, RpcMessage};
pub use server::{LanguageServer, NESSA_LSP_VERSION};
