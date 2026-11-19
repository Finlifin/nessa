//! LSP 3.17 JSON-RPC 2.0 server state machine and request dispatcher for `nessa-lsp`.

use std::collections::{HashSet, VecDeque};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use crate::analysis::{LspDiagnostic, ServerConfig, WorkspaceAnalyzer, normalize_path};
use crate::index::{semantic_token_modifier, semantic_token_type};
use crate::json::JsonValue;
use crate::position::{LspPosition, LspRange, path_to_uri, uri_to_path};
use crate::rpc::{
    ERR_INVALID_PARAMS, ERR_INVALID_REQUEST, ERR_METHOD_NOT_FOUND, REQUEST_CANCELLED, RequestId,
    RpcMessage, SERVER_NOT_INITIALIZED, read_rpc_message, send_error, send_notification,
    send_response,
};

pub const NESSA_LSP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct LanguageServer {
    pub analyzer: WorkspaceAnalyzer,
    initialize_received: bool,
    initialized: bool,
    shutdown_requested: bool,
    cancelled_ids: HashSet<RequestId>,
    published_files: HashSet<PathBuf>,
    dirty_files: Vec<PathBuf>,
}

const STDIO_DIAG_DEBOUNCE_MS: u64 = 450;

impl Default for LanguageServer {
    fn default() -> Self {
        Self::new(ServerConfig::default())
    }
}

impl LanguageServer {
    pub fn new(config: ServerConfig) -> Self {
        Self {
            analyzer: WorkspaceAnalyzer::new(config),
            initialize_received: false,
            initialized: false,
            shutdown_requested: false,
            cancelled_ids: HashSet::new(),
            published_files: HashSet::new(),
            dirty_files: Vec::new(),
        }
    }

    /// Run the main JSON-RPC message loop synchronously over `reader` until `exit` or EOF.
    /// Returns the process exit code (`0` after clean `shutdown`, `1` if `exit` arrived without `shutdown`).
    pub fn run<R: BufRead, W: Write>(&mut self, reader: &mut R, writer: &mut W) -> io::Result<i32> {
        while let Some(msg) = read_rpc_message(reader)? {
            match msg {
                RpcMessage::Notification { method, params } => {
                    if method == "exit" {
                        self.flush_dirty_diagnostics(writer)?;
                        return Ok(if self.shutdown_requested { 0 } else { 1 });
                    }
                    self.handle_notification(&method, &params, writer, false)?;
                }
                RpcMessage::Request { id, method, params } => {
                    if self.cancelled_ids.remove(&id) {
                        send_error(writer, id, REQUEST_CANCELLED, "request cancelled")?;
                        continue;
                    }
                    if self.shutdown_requested {
                        send_error(writer, id, ERR_INVALID_REQUEST, "server is shutting down")?;
                        continue;
                    }
                    if !self.initialize_received && method != "initialize" {
                        send_error(
                            writer,
                            id,
                            SERVER_NOT_INITIALIZED,
                            "server has not received `initialize` request",
                        )?;
                        continue;
                    }
                    self.handle_request(id, &method, &params, writer, false)?;
                }
                RpcMessage::Response { .. } => {}
            }
        }
        self.flush_dirty_diagnostics(writer)?;
        Ok(0)
    }

    /// Run the live `--stdio` JSON-RPC loop with `textDocument/didChange` coalescing and
    /// non-blocking interactive completion / signatureHelp during rapid typing bursts.
    pub fn run_stdio<W: Write>(&mut self, writer: &mut W) -> io::Result<i32> {
        let (tx, rx) = mpsc::channel::<io::Result<RpcMessage>>();
        std::thread::spawn(move || {
            let stdin = io::stdin();
            let mut reader = BufReader::new(stdin.lock());
            loop {
                match read_rpc_message(&mut reader) {
                    Ok(Some(msg)) => {
                        let is_exit = matches!(&msg, RpcMessage::Notification { method, .. } if method == "exit");
                        if tx.send(Ok(msg)).is_err() || is_exit {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(err) => {
                        let _ = tx.send(Err(err));
                        break;
                    }
                }
            }
        });
        self.run_coalesced_channel(rx, writer, Duration::from_millis(STDIO_DIAG_DEBOUNCE_MS))
    }

    pub fn run_coalesced_channel<W: Write>(
        &mut self,
        rx: mpsc::Receiver<io::Result<RpcMessage>>,
        writer: &mut W,
        debounce: Duration,
    ) -> io::Result<i32> {
        let mut queue: VecDeque<RpcMessage> = VecDeque::new();
        loop {
            if queue.is_empty() {
                if self.dirty_files.is_empty() {
                    match rx.recv() {
                        Ok(item) => queue.push_back(item?),
                        Err(_) => break,
                    }
                } else {
                    match rx.recv_timeout(debounce) {
                        Ok(item) => queue.push_back(item?),
                        Err(RecvTimeoutError::Timeout) => {
                            self.flush_dirty_diagnostics(writer)?;
                            continue;
                        }
                        Err(RecvTimeoutError::Disconnected) => {
                            self.flush_dirty_diagnostics(writer)?;
                            break;
                        }
                    }
                }
            }

            // Drain any additional messages already queued in the pipe so typing bursts coalesce!
            while let Ok(item) = rx.try_recv() {
                queue.push_back(item?);
            }

            // Pre-scan queued `$/cancelRequest` notifications so cancelled requests are skipped before work!
            for msg in &queue {
                if let RpcMessage::Notification { method, params } = msg
                    && method == "$/cancelRequest"
                    && let Some(id_val) = params.get("id")
                    && let Some(id) = RequestId::from_json(id_val)
                {
                    self.cancelled_ids.insert(id);
                }
            }

            while let Some(msg) = queue.pop_front() {
                match msg {
                    RpcMessage::Notification { method, params } => {
                        if method == "exit" {
                            return Ok(if self.shutdown_requested { 0 } else { 1 });
                        }
                        if method == "$/cancelRequest" {
                            continue;
                        }
                        self.handle_notification(&method, &params, writer, true)?;
                    }
                    RpcMessage::Request { id, method, params } => {
                        if self.cancelled_ids.remove(&id) {
                            send_error(writer, id, REQUEST_CANCELLED, "request cancelled")?;
                            continue;
                        }
                        if self.shutdown_requested {
                            send_error(writer, id, ERR_INVALID_REQUEST, "server is shutting down")?;
                            continue;
                        }
                        if !self.initialize_received && method != "initialize" {
                            send_error(
                                writer,
                                id,
                                SERVER_NOT_INITIALIZED,
                                "server has not received `initialize` request",
                            )?;
                            continue;
                        }
                        let prefer_fast = !self.dirty_files.is_empty()
                            && matches!(
                                method.as_str(),
                                "textDocument/completion"
                                    | "textDocument/signatureHelp"
                                    | "textDocument/hover"
                                    | "textDocument/definition"
                                    | "textDocument/declaration"
                                    | "textDocument/typeDefinition"
                                    | "textDocument/implementation"
                                    | "textDocument/documentSymbol"
                                    | "textDocument/foldingRange"
                            );
                        if !prefer_fast && method != "shutdown" && method != "initialize" {
                            self.flush_dirty_diagnostics(writer)?;
                        }
                        self.handle_request(id, &method, &params, writer, prefer_fast)?;
                    }
                    RpcMessage::Response { .. } => {}
                }
                // Check for newly arrived burst messages before processing remaining items.
                while let Ok(item) = rx.try_recv() {
                    let next_msg = item?;
                    if let RpcMessage::Notification {
                        ref method,
                        ref params,
                    } = next_msg
                        && method == "$/cancelRequest"
                        && let Some(id_val) = params.get("id")
                        && let Some(id) = RequestId::from_json(id_val)
                    {
                        self.cancelled_ids.insert(id);
                    }
                    queue.push_back(next_msg);
                }
            }
        }
        self.flush_dirty_diagnostics(writer)?;
        Ok(0)
    }

    fn flush_dirty_diagnostics<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.dirty_files.is_empty() {
            return Ok(());
        }
        let pending = std::mem::take(&mut self.dirty_files);
        let mut seen = HashSet::new();
        for path in pending {
            if seen.insert(path.clone()) {
                self.publish_package_diagnostics(&path, writer)?;
            }
        }
        Ok(())
    }

    fn handle_notification<W: Write>(
        &mut self,
        method: &str,
        params: &JsonValue,
        writer: &mut W,
        defer_change_reanalysis: bool,
    ) -> io::Result<()> {
        match method {
            "initialized" => {
                self.initialized = true;
            }
            "$/cancelRequest" => {
                if let Some(id_val) = params.get("id")
                    && let Some(id) = RequestId::from_json(id_val)
                {
                    self.cancelled_ids.insert(id);
                }
            }
            "$/setTrace" => {}
            "workspace/didChangeConfiguration" => {
                if let Some(settings) = params.get("settings") {
                    let nessa_cfg = settings.get("nessa").unwrap_or(settings);
                    if let Some(sr) = nessa_cfg.get_str("sysroot") {
                        let trimmed = sr.trim();
                        if trimmed.is_empty() {
                            self.analyzer.set_sysroot_override(None);
                        } else {
                            self.analyzer
                                .set_sysroot_override(Some(PathBuf::from(trimmed)));
                        }
                    }
                    if let Some(fmt_en) = nessa_cfg
                        .get("format")
                        .and_then(|f| f.get_bool("enable"))
                        .or_else(|| nessa_cfg.get_bool("formatEnable"))
                    {
                        self.analyzer.config.format_enabled = fmt_en;
                    }
                    if let Some(priv_arr) = nessa_cfg.get_array("privilegedPackages") {
                        self.analyzer.config.privileged_packages = priv_arr
                            .iter()
                            .filter_map(JsonValue::as_str)
                            .map(str::to_owned)
                            .collect();
                    }
                }
                self.analyzer.invalidate_all();
                self.republish_open_diagnostics(writer)?;
            }
            "workspace/didChangeWorkspaceFolders" => {
                if let Some(event) = params.get("event") {
                    if let Some(removed) = event.get_array("removed") {
                        for rem in removed {
                            if let Some(uri) = rem.get_str("uri")
                                && let Some(p) = uri_to_path(uri)
                            {
                                let norm = normalize_path(&p);
                                self.analyzer.workspace_roots.retain(|r| r != &norm);
                            }
                        }
                    }
                    if let Some(added) = event.get_array("added") {
                        for add in added {
                            if let Some(uri) = add.get_str("uri")
                                && let Some(p) = uri_to_path(uri)
                            {
                                let norm = normalize_path(&p);
                                if !self.analyzer.workspace_roots.contains(&norm) {
                                    self.analyzer.workspace_roots.push(norm);
                                }
                            }
                        }
                    }
                }
                self.analyzer.invalidate_all();
                self.republish_open_diagnostics(writer)?;
            }
            "workspace/didChangeWatchedFiles" => {
                self.analyzer.invalidate_all();
                self.republish_open_diagnostics(writer)?;
            }
            "textDocument/didOpen" => {
                if let Some(td) = params.get("textDocument")
                    && let Some(uri) = td.get_str("uri")
                    && let Some(text) = td.get_str("text")
                {
                    let version = td.get_i64("version").unwrap_or(0);
                    if let Some(path) = self.analyzer.open_document(uri, version, text.to_owned()) {
                        self.dirty_files.retain(|p| p != &path);
                        self.publish_package_diagnostics(&path, writer)?;
                    }
                }
            }
            "textDocument/didChange" => {
                if let Some(td) = params.get("textDocument")
                    && let Some(uri) = td.get_str("uri")
                    && let Some(changes) = params.get_array("contentChanges")
                {
                    let version = td.get_i64("version").unwrap_or(0);
                    if defer_change_reanalysis {
                        if let Some(path) =
                            self.analyzer.stage_document_change(uri, version, changes)
                            && !self.dirty_files.contains(&path)
                        {
                            self.dirty_files.push(path);
                        }
                    } else if let Some(path) = self.analyzer.change_document(uri, version, changes)
                    {
                        self.publish_package_diagnostics(&path, writer)?;
                    }
                }
            }
            "textDocument/didSave" => {
                if let Some(td) = params.get("textDocument")
                    && let Some(uri) = td.get_str("uri")
                {
                    let text = params.get_str("text").map(str::to_owned);
                    if let Some(path) = self.analyzer.save_document(uri, text) {
                        self.dirty_files.retain(|p| p != &path);
                        self.publish_package_diagnostics(&path, writer)?;
                    }
                }
            }
            "textDocument/didClose" => {
                if let Some(td) = params.get("textDocument")
                    && let Some(uri) = td.get_str("uri")
                    && let Some(path) = self.analyzer.close_document(uri)
                {
                    self.dirty_files.retain(|p| p != &path);
                    if path.exists() {
                        self.publish_package_diagnostics(&path, writer)?;
                    } else {
                        self.published_files.remove(&path);
                        self.publish_file_diagnostics(&path, &[], writer)?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_request<W: Write>(
        &mut self,
        id: RequestId,
        method: &str,
        params: &JsonValue,
        writer: &mut W,
        prefer_fast_interactive: bool,
    ) -> io::Result<()> {
        match method {
            "initialize" => {
                self.initialize_received = true;
                self.apply_initialize_params(params);
                let result = build_initialize_result();
                send_response(writer, id, result)?;
            }
            "shutdown" => {
                self.shutdown_requested = true;
                send_response(writer, id, JsonValue::Null)?;
            }
            "textDocument/hover" => {
                let Some((path, pos)) = extract_doc_pos(params) else {
                    return send_error(
                        writer,
                        id,
                        ERR_INVALID_PARAMS,
                        "invalid textDocumentPosition",
                    );
                };
                let hover = if prefer_fast_interactive {
                    self.analyzer.hover_interactive(&path, pos)
                } else {
                    self.analyzer.hover(&path, pos)
                }
                .unwrap_or(JsonValue::Null);
                send_response(writer, id, hover)?;
            }
            "textDocument/definition" | "textDocument/declaration" => {
                let Some((path, pos)) = extract_doc_pos(params) else {
                    return send_error(
                        writer,
                        id,
                        ERR_INVALID_PARAMS,
                        "invalid textDocumentPosition",
                    );
                };
                let locs = if prefer_fast_interactive {
                    self.analyzer.goto_definition_interactive(&path, pos)
                } else {
                    self.analyzer.goto_definition(&path, pos)
                };
                let resp = if locs.is_empty() {
                    JsonValue::Null
                } else {
                    JsonValue::Array(locs)
                };
                send_response(writer, id, resp)?;
            }
            "textDocument/typeDefinition" => {
                let Some((path, pos)) = extract_doc_pos(params) else {
                    return send_error(
                        writer,
                        id,
                        ERR_INVALID_PARAMS,
                        "invalid textDocumentPosition",
                    );
                };
                let locs = if prefer_fast_interactive {
                    self.analyzer.goto_type_definition_interactive(&path, pos)
                } else {
                    self.analyzer.goto_type_definition(&path, pos)
                };
                let resp = if locs.is_empty() {
                    JsonValue::Null
                } else {
                    JsonValue::Array(locs)
                };
                send_response(writer, id, resp)?;
            }
            "textDocument/implementation" => {
                let Some((path, pos)) = extract_doc_pos(params) else {
                    return send_error(
                        writer,
                        id,
                        ERR_INVALID_PARAMS,
                        "invalid textDocumentPosition",
                    );
                };
                let locs = if prefer_fast_interactive {
                    self.analyzer.find_implementations_interactive(&path, pos)
                } else {
                    self.analyzer.find_implementations(&path, pos)
                };
                send_response(writer, id, JsonValue::Array(locs))?;
            }
            "textDocument/references" => {
                let Some((path, pos)) = extract_doc_pos(params) else {
                    return send_error(
                        writer,
                        id,
                        ERR_INVALID_PARAMS,
                        "invalid textDocumentPosition",
                    );
                };
                let include_decl = params
                    .get("context")
                    .and_then(|c| c.get_bool("includeDeclaration"))
                    .unwrap_or(true);
                let refs = self.analyzer.find_references(&path, pos, include_decl);
                send_response(writer, id, JsonValue::Array(refs))?;
            }
            "textDocument/documentHighlight" => {
                let Some((path, pos)) = extract_doc_pos(params) else {
                    return send_error(
                        writer,
                        id,
                        ERR_INVALID_PARAMS,
                        "invalid textDocumentPosition",
                    );
                };
                let hl = self.analyzer.document_highlights(&path, pos);
                send_response(writer, id, JsonValue::Array(hl))?;
            }
            "textDocument/prepareRename" => {
                let Some((path, pos)) = extract_doc_pos(params) else {
                    return send_error(
                        writer,
                        id,
                        ERR_INVALID_PARAMS,
                        "invalid textDocumentPosition",
                    );
                };
                let prep = self
                    .analyzer
                    .prepare_rename(&path, pos)
                    .unwrap_or(JsonValue::Null);
                send_response(writer, id, prep)?;
            }
            "textDocument/rename" => {
                let Some((path, pos)) = extract_doc_pos(params) else {
                    return send_error(
                        writer,
                        id,
                        ERR_INVALID_PARAMS,
                        "invalid textDocumentPosition",
                    );
                };
                let Some(new_name) = params.get_str("newName") else {
                    return send_error(writer, id, ERR_INVALID_PARAMS, "missing newName");
                };
                match self.analyzer.rename(&path, pos, new_name) {
                    Ok(edit) => send_response(writer, id, edit)?,
                    Err(msg) => send_error(writer, id, ERR_INVALID_PARAMS, &msg)?,
                }
            }
            "textDocument/completion" => {
                let Some((path, pos)) = extract_doc_pos(params) else {
                    return send_error(
                        writer,
                        id,
                        ERR_INVALID_PARAMS,
                        "invalid textDocumentPosition",
                    );
                };
                let items = if prefer_fast_interactive {
                    self.analyzer.complete_interactive(&path, pos)
                } else {
                    self.analyzer.complete(&path, pos)
                };
                let result = JsonValue::object([
                    ("isIncomplete", JsonValue::Bool(false)),
                    ("items", JsonValue::Array(items)),
                ]);
                send_response(writer, id, result)?;
            }
            "textDocument/signatureHelp" => {
                let Some((path, pos)) = extract_doc_pos(params) else {
                    return send_error(
                        writer,
                        id,
                        ERR_INVALID_PARAMS,
                        "invalid textDocumentPosition",
                    );
                };
                let sh = if prefer_fast_interactive {
                    self.analyzer.signature_help_interactive(&path, pos)
                } else {
                    self.analyzer.signature_help(&path, pos)
                }
                .unwrap_or(JsonValue::Null);
                send_response(writer, id, sh)?;
            }
            "textDocument/documentSymbol" => {
                let Some(path) = extract_doc_path(params) else {
                    return send_error(writer, id, ERR_INVALID_PARAMS, "invalid textDocument");
                };
                let syms = if prefer_fast_interactive {
                    self.analyzer.document_symbols_interactive(&path)
                } else {
                    self.analyzer.document_symbols(&path)
                };
                send_response(writer, id, JsonValue::Array(syms))?;
            }
            "workspace/symbol" => {
                let query = params.get_str("query").unwrap_or("");
                let syms = self.analyzer.workspace_symbols(query);
                send_response(writer, id, JsonValue::Array(syms))?;
            }
            "textDocument/semanticTokens/full" => {
                let Some(path) = extract_doc_path(params) else {
                    return send_error(writer, id, ERR_INVALID_PARAMS, "invalid textDocument");
                };
                let raw = self.analyzer.semantic_tokens(&path);
                let arr: Vec<JsonValue> = raw.into_iter().map(JsonValue::from).collect();
                send_response(
                    writer,
                    id,
                    JsonValue::object([("data", JsonValue::Array(arr))]),
                )?;
            }
            "textDocument/foldingRange" => {
                let Some(path) = extract_doc_path(params) else {
                    return send_error(writer, id, ERR_INVALID_PARAMS, "invalid textDocument");
                };
                let folds = self.analyzer.folding_ranges(&path);
                send_response(writer, id, JsonValue::Array(folds))?;
            }
            "textDocument/formatting" => {
                if !self.analyzer.config.format_enabled {
                    return send_response(writer, id, JsonValue::Array(Vec::new()));
                }
                let Some(path) = extract_doc_path(params) else {
                    return send_error(writer, id, ERR_INVALID_PARAMS, "invalid textDocument");
                };
                let tab_spaces = params
                    .get("options")
                    .and_then(|o| o.get_u32("tabSize"))
                    .unwrap_or(4);
                match self.analyzer.format_document(&path, tab_spaces) {
                    Ok(edits) => send_response(writer, id, JsonValue::Array(edits))?,
                    Err(_) => send_response(writer, id, JsonValue::Array(Vec::new()))?,
                }
            }
            "textDocument/rangeFormatting" => {
                if !self.analyzer.config.format_enabled {
                    return send_response(writer, id, JsonValue::Array(Vec::new()));
                }
                let Some(path) = extract_doc_path(params) else {
                    return send_error(writer, id, ERR_INVALID_PARAMS, "invalid textDocument");
                };
                let Some(range_val) = params.get("range") else {
                    return send_error(writer, id, ERR_INVALID_PARAMS, "missing range");
                };
                let Some(range) = LspRange::from_json(range_val) else {
                    return send_error(writer, id, ERR_INVALID_PARAMS, "invalid range");
                };
                let tab_spaces = params
                    .get("options")
                    .and_then(|o| o.get_u32("tabSize"))
                    .unwrap_or(4);
                match self.analyzer.format_range(&path, range, tab_spaces) {
                    Ok(edits) => send_response(writer, id, JsonValue::Array(edits))?,
                    Err(_) => send_response(writer, id, JsonValue::Array(Vec::new()))?,
                }
            }
            "textDocument/codeAction" => {
                let Some(path) = extract_doc_path(params) else {
                    return send_error(writer, id, ERR_INVALID_PARAMS, "invalid textDocument");
                };
                let range = params
                    .get("range")
                    .and_then(LspRange::from_json)
                    .unwrap_or_default();
                let actions = self.analyzer.code_actions(&path, range);
                send_response(writer, id, JsonValue::Array(actions))?;
            }
            _ => {
                send_error(
                    writer,
                    id,
                    ERR_METHOD_NOT_FOUND,
                    &format!("unsupported method `{method}`"),
                )?;
            }
        }
        Ok(())
    }

    fn apply_initialize_params(&mut self, params: &JsonValue) {
        let mut roots = Vec::new();
        if let Some(folders) = params.get_array("workspaceFolders") {
            for f in folders {
                if let Some(uri) = f.get_str("uri")
                    && let Some(p) = uri_to_path(uri)
                {
                    roots.push(p);
                }
            }
        }
        if roots.is_empty() {
            if let Some(uri) = params.get_str("rootUri")
                && let Some(p) = uri_to_path(uri)
            {
                roots.push(p);
            } else if let Some(rp) = params.get_str("rootPath") {
                roots.push(PathBuf::from(rp));
            }
        }
        if !roots.is_empty() {
            self.analyzer.set_workspace_roots(roots);
        }

        if let Some(init_opts) = params.get("initializationOptions") {
            if let Some(sr) = init_opts.get_str("sysroot") {
                let trimmed = sr.trim();
                if !trimmed.is_empty() {
                    self.analyzer
                        .set_sysroot_override(Some(PathBuf::from(trimmed)));
                }
            }
            if let Some(fmt_en) = init_opts.get_bool("formatEnable") {
                self.analyzer.config.format_enabled = fmt_en;
            }
            if let Some(priv_arr) = init_opts.get_array("privilegedPackages") {
                for item in priv_arr {
                    if let Some(pkg) = item.as_str() {
                        self.analyzer
                            .config
                            .privileged_packages
                            .insert(pkg.to_owned());
                    }
                }
            }
        }
    }

    fn publish_package_diagnostics<W: Write>(
        &mut self,
        file: &Path,
        writer: &mut W,
    ) -> io::Result<()> {
        let norm_file = normalize_path(file);
        let snap = self.analyzer.ensure_snapshot(&norm_file).clone();

        let mut targets: HashSet<PathBuf> = snap.diagnostics.keys().cloned().collect();
        for member in &snap.member_files {
            targets.insert(member.clone());
        }
        for prev in &self.published_files {
            if prev.starts_with(&snap.root_path) || snap.member_files.contains(prev) {
                targets.insert(prev.clone());
            }
        }
        targets.insert(norm_file);

        let mut sorted: Vec<PathBuf> = targets.into_iter().collect();
        sorted.sort();

        for target in sorted {
            let diags = snap
                .diagnostics
                .get(&target)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let was_published = self.published_files.contains(&target);
            let is_open = self.analyzer.open_docs.contains_key(&target);
            if !diags.is_empty() {
                self.published_files.insert(target.clone());
                self.publish_file_diagnostics(&target, diags, writer)?;
            } else if was_published || is_open {
                self.published_files.remove(&target);
                self.publish_file_diagnostics(&target, &[], writer)?;
            }
        }
        Ok(())
    }

    fn republish_open_diagnostics<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        let open_paths: Vec<PathBuf> = self.analyzer.open_docs.keys().cloned().collect();
        for p in open_paths {
            self.publish_package_diagnostics(&p, writer)?;
        }
        Ok(())
    }

    fn publish_file_diagnostics<W: Write>(
        &self,
        file: &Path,
        diags: &[LspDiagnostic],
        writer: &mut W,
    ) -> io::Result<()> {
        let uri = path_to_uri(file);
        let items: Vec<JsonValue> = diags.iter().map(LspDiagnostic::to_json).collect();
        let mut fields = vec![
            ("uri", JsonValue::from(uri)),
            ("diagnostics", JsonValue::Array(items)),
        ];
        if let Some(doc) = self.analyzer.open_docs.get(file) {
            fields.push(("version", JsonValue::from(doc.version)));
        }
        send_notification(
            writer,
            "textDocument/publishDiagnostics",
            JsonValue::object(fields),
        )
    }
}

fn extract_doc_path(params: &JsonValue) -> Option<PathBuf> {
    let uri = params.get("textDocument")?.get_str("uri")?;
    uri_to_path(uri)
}

fn extract_doc_pos(params: &JsonValue) -> Option<(PathBuf, LspPosition)> {
    let path = extract_doc_path(params)?;
    let pos = LspPosition::from_json(params.get("position")?)?;
    Some((path, pos))
}

fn build_initialize_result() -> JsonValue {
    let token_types: Vec<JsonValue> = semantic_token_type::LEGEND
        .iter()
        .map(|&s| JsonValue::from(s))
        .collect();
    let token_mods: Vec<JsonValue> = semantic_token_modifier::LEGEND
        .iter()
        .map(|&s| JsonValue::from(s))
        .collect();

    JsonValue::object([
        (
            "capabilities",
            JsonValue::object([
                ("positionEncoding", JsonValue::from("utf-16")),
                (
                    "textDocumentSync",
                    JsonValue::object([
                        ("openClose", JsonValue::Bool(true)),
                        // 2 = Incremental (also handles full-buffer edits when `range` is omitted)
                        ("change", JsonValue::from(2u32)),
                        (
                            "save",
                            JsonValue::object([("includeText", JsonValue::Bool(false))]),
                        ),
                    ]),
                ),
                ("hoverProvider", JsonValue::Bool(true)),
                ("definitionProvider", JsonValue::Bool(true)),
                ("declarationProvider", JsonValue::Bool(true)),
                ("typeDefinitionProvider", JsonValue::Bool(true)),
                ("implementationProvider", JsonValue::Bool(true)),
                ("referencesProvider", JsonValue::Bool(true)),
                ("documentHighlightProvider", JsonValue::Bool(true)),
                ("documentSymbolProvider", JsonValue::Bool(true)),
                ("workspaceSymbolProvider", JsonValue::Bool(true)),
                ("codeActionProvider", JsonValue::Bool(true)),
                (
                    "renameProvider",
                    JsonValue::object([("prepareProvider", JsonValue::Bool(true))]),
                ),
                (
                    "completionProvider",
                    JsonValue::object([
                        ("resolveProvider", JsonValue::Bool(false)),
                        (
                            "triggerCharacters",
                            JsonValue::array([
                                JsonValue::from("."),
                                JsonValue::from("@"),
                                JsonValue::from("'"),
                                JsonValue::from("%"),
                                JsonValue::from(":"),
                            ]),
                        ),
                    ]),
                ),
                (
                    "signatureHelpProvider",
                    JsonValue::object([(
                        "triggerCharacters",
                        JsonValue::array([JsonValue::from("("), JsonValue::from(",")]),
                    )]),
                ),
                ("foldingRangeProvider", JsonValue::Bool(true)),
                ("documentFormattingProvider", JsonValue::Bool(true)),
                ("documentRangeFormattingProvider", JsonValue::Bool(true)),
                (
                    "semanticTokensProvider",
                    JsonValue::object([
                        (
                            "legend",
                            JsonValue::object([
                                ("tokenTypes", JsonValue::Array(token_types)),
                                ("tokenModifiers", JsonValue::Array(token_mods)),
                            ]),
                        ),
                        ("full", JsonValue::Bool(true)),
                    ]),
                ),
                (
                    "workspace",
                    JsonValue::object([(
                        "workspaceFolders",
                        JsonValue::object([
                            ("supported", JsonValue::Bool(true)),
                            ("changeNotifications", JsonValue::Bool(true)),
                        ]),
                    )]),
                ),
            ]),
        ),
        (
            "serverInfo",
            JsonValue::object([
                ("name", JsonValue::from("nessa-lsp")),
                ("version", JsonValue::from(NESSA_LSP_VERSION)),
            ]),
        ),
    ])
}
