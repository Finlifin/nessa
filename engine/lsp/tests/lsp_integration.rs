//! End-to-end JSON-RPC 2.0 client-server integration tests for `nessa-lsp`.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use nessa_lsp::rpc::{read_rpc_message, write_message};
use nessa_lsp::{
    JsonValue, LanguageServer, LineIndex, LspPosition, LspRange, RequestId, RpcMessage,
    ServerConfig, path_to_uri, run_cli_with_config, uri_to_path,
};

struct TempWorkspace(PathBuf);

impl TempWorkspace {
    fn new(label: &str) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        loop {
            let dir = std::env::temp_dir().join(format!(
                "nessa-lsp-test-{label}-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&dir) {
                Ok(()) => return Self(dir),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("failed to create temp test dir: {e}"),
            }
        }
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, rel: &str, content: &str) -> PathBuf {
        let full = self.0.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(&full, content).expect("write test file");
        fs::canonicalize(&full).unwrap_or(full)
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct RpcWireBuilder {
    bytes: Vec<u8>,
}

impl RpcWireBuilder {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn request(&mut self, id: i64, method: &str, params: JsonValue) -> &mut Self {
        let payload = JsonValue::object([
            ("jsonrpc", JsonValue::from("2.0")),
            ("id", JsonValue::from(id)),
            ("method", JsonValue::from(method)),
            ("params", params),
        ]);
        write_message(&mut self.bytes, &payload).unwrap();
        self
    }

    fn notify(&mut self, method: &str, params: JsonValue) -> &mut Self {
        let payload = JsonValue::object([
            ("jsonrpc", JsonValue::from("2.0")),
            ("method", JsonValue::from(method)),
            ("params", params),
        ]);
        write_message(&mut self.bytes, &payload).unwrap();
        self
    }

    fn run_with_config(self, config: ServerConfig) -> (i32, Vec<RpcMessage>) {
        let mut server = LanguageServer::new(config);
        let mut reader = Cursor::new(self.bytes);
        let mut output = Vec::new();
        let exit_code = server.run(&mut reader, &mut output).expect("server run");

        let mut out_reader = Cursor::new(output);
        let mut messages = Vec::new();
        while let Some(msg) = read_rpc_message(&mut out_reader).expect("decode server output") {
            messages.push(msg);
        }
        (exit_code, messages)
    }

    fn run(self) -> (i32, Vec<RpcMessage>) {
        self.run_with_config(ServerConfig::default())
    }
}

fn find_response(messages: &[RpcMessage], target_id: i64) -> &RpcMessage {
    messages
        .iter()
        .find(|m| matches!(m, RpcMessage::Response { id: RequestId::Int(id), .. } if *id == target_id))
        .unwrap_or_else(|| panic!("missing response for id {target_id} in {messages:?}"))
}

fn find_ok_result(messages: &[RpcMessage], target_id: i64) -> &JsonValue {
    match find_response(messages, target_id) {
        RpcMessage::Response {
            result: Some(res),
            error: None,
            ..
        } => res,
        other => panic!("expected ok response for id {target_id}, got {other:?}"),
    }
}

fn pos_of_nth(text: &str, needle: &str, nth: usize) -> LspPosition {
    let mut start = 0usize;
    for i in 0..=nth {
        let rel = text[start..]
            .find(needle)
            .unwrap_or_else(|| panic!("needle `{needle}` occurrence {nth} not found"));
        let abs = start + rel;
        if i == nth {
            return LineIndex::new(text).offset_to_position(text, abs);
        }
        start = abs + needle.len();
    }
    unreachable!()
}

fn pos_after(text: &str, needle: &str) -> LspPosition {
    let off = text
        .find(needle)
        .unwrap_or_else(|| panic!("needle `{needle}` not found"))
        + needle.len();
    LineIndex::new(text).offset_to_position(text, off)
}

fn setup_demo_package(ws: &TempWorkspace) -> (PathBuf, PathBuf, PathBuf) {
    ws.write(
        "package.toml",
        r#"[package]
name = "demo_pkg"
domain = "test.org"
version = "0.1.0"
type = "exe"
"#,
    );

    let geometry_src = r#"-- Geometry primitives and traits.
pub struct Point {
    pub x: i64,
    pub y: i64,
}

pub enum Color {
    pub red,
    pub green,
    pub custom_rgb(r: u8, g: u8, b: u8),
}

pub trait Renderable {
    fn render_label(self) -> String
}

impl Renderable for Point {
    pub fn render_label(self) -> String = "point"
}

impl Point {
    --- Returns a translated copy of this point.
    pub fn shifted(self, dx: i64, .dy: i64 = 0) -> Point {
        Point { x: self.x + dx, y: self.y + dy }
    }
}
"#;
    let geometry_path = ws.write("src/geometry.ns", geometry_src);

    let effects_src = r#"pub enum ConfigError {
    pub MissingKey,
    pub InvalidPort(port: i64),
}

pub effect log_event(msg: String) -> Unit

--- Loads a server port with logging capability and typed errors.
pub fn load_port(key: String) -> #log_event !ConfigError i64 {
    log_event(key)
    if key == "" {
        return error ConfigError.MissingKey
    }
    8080
}

pub fn configure_server(host: String, .timeout_ms: i64 = 5000) -> String = host
"#;
    let effects_path = ws.write("src/effects.ns", effects_src);

    let main_src = r#"use std.io

use @effects.{ConfigError, configure_server, load_port, log_event}
use @geometry.{Color, Point, Renderable}

fn make_origin() -> Point {
    Point { x: 0, y: 0 }
}

fn main() {
    -- 状态标签 🚀
    let origin = make_origin()
    let moved = origin.shifted(10, dy = 20)
    let shade = Color.custom_rgb(1, 2, 3)
    let label = moved.render_label()
    var counter = moved.x
    counter += 5
    let final_total = counter + moved.y
    println(label)
}
"#;
    let main_path = ws.write("src/main.ns", main_src);
    (geometry_path, effects_path, main_path)
}

#[test]
fn lifecycle_handshake_cancellation_and_shutdown_enforcement() {
    let mut wire = RpcWireBuilder::new();
    // Pre-initialize request must fail with SERVER_NOT_INITIALIZED (-32002).
    wire.request(
        1,
        "workspace/symbol",
        JsonValue::object([("query", JsonValue::from(""))]),
    );
    wire.request(
        2,
        "initialize",
        JsonValue::object([("rootUri", JsonValue::Null)]),
    );
    wire.notify("initialized", JsonValue::empty_object());
    // Cancel request #3 before it is processed? Note: in single-threaded stream, `$/cancelRequest`
    // before request #3 cancels #3 cleanly!
    wire.notify(
        "$/cancelRequest",
        JsonValue::object([("id", JsonValue::from(3))]),
    );
    wire.request(
        3,
        "workspace/symbol",
        JsonValue::object([("query", JsonValue::from(""))]),
    );
    wire.request(4, "shutdown", JsonValue::Null);
    // Request after shutdown must fail with INVALID_REQUEST (-32600).
    wire.request(
        5,
        "workspace/symbol",
        JsonValue::object([("query", JsonValue::from(""))]),
    );
    wire.notify("exit", JsonValue::Null);

    let (code, msgs) = wire.run();
    assert_eq!(code, 0);

    match find_response(&msgs, 1) {
        RpcMessage::Response {
            error: Some(err), ..
        } => assert_eq!(err.code, -32002),
        other => panic!("expected -32002, got {other:?}"),
    }

    let init_res = find_ok_result(&msgs, 2);
    assert_eq!(
        init_res.get("serverInfo").and_then(|s| s.get_str("name")),
        Some("nessa-lsp")
    );
    assert!(
        init_res
            .get("capabilities")
            .and_then(|c| c.get_bool("hoverProvider"))
            == Some(true)
    );

    match find_response(&msgs, 3) {
        RpcMessage::Response {
            error: Some(err), ..
        } => assert_eq!(err.code, -32800),
        other => panic!("expected -32800 cancelled, got {other:?}"),
    }

    assert!(find_ok_result(&msgs, 4).is_null());

    match find_response(&msgs, 5) {
        RpcMessage::Response {
            error: Some(err), ..
        } => assert_eq!(err.code, -32600),
        other => panic!("expected -32600 after shutdown, got {other:?}"),
    }

    // Exit without shutdown returns 1.
    let mut abrupt = RpcWireBuilder::new();
    abrupt.notify("exit", JsonValue::Null);
    let (abrupt_code, _) = abrupt.run();
    assert_eq!(abrupt_code, 1);
}

#[test]
fn multifile_vfs_utf16_incremental_edits_and_cross_module_diagnostics_clearing() {
    let ws = TempWorkspace::new("vfs-utf16");
    let (geometry_path, _, main_path) = setup_demo_package(&ws);
    let main_uri = path_to_uri(&main_path);
    let geom_uri = path_to_uri(&geometry_path);

    let initial_main =
        "use @geometry.Point\nfn main() {\n    let tag = \"中文🚀\"; let bad: bool = 123\n}\n";
    // On line 2: `    let tag = "中文🚀"; let bad: bool = 123`
    // UTF-16 count before `123`:
    // `    let tag = "` (15) + `中文` (2) + `🚀` (2) + `"; let bad: bool = ` (19) = 38 UTF-16 units!
    let mut wire = RpcWireBuilder::new();
    wire.request(
        1,
        "initialize",
        JsonValue::object([("rootUri", JsonValue::from(path_to_uri(ws.path())))]),
    );
    wire.notify("initialized", JsonValue::empty_object());
    wire.notify(
        "textDocument/didOpen",
        JsonValue::object([(
            "textDocument",
            JsonValue::object([
                ("uri", JsonValue::from(main_uri.as_str())),
                ("languageId", JsonValue::from("nessa")),
                ("version", JsonValue::from(1)),
                ("text", JsonValue::from(initial_main)),
            ]),
        )]),
    );
    // Incremental UTF-16 edit replacing `123` at (2, 38)..(2, 41) with `true`.
    wire.notify(
        "textDocument/didChange",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([
                    ("uri", JsonValue::from(main_uri.as_str())),
                    ("version", JsonValue::from(2)),
                ]),
            ),
            (
                "contentChanges",
                JsonValue::Array(vec![JsonValue::object([
                    (
                        "range",
                        LspRange::new(LspPosition::new(2, 38), LspPosition::new(2, 41)).to_json(),
                    ),
                    ("text", JsonValue::from("true")),
                ])]),
            ),
        ]),
    );
    // Open `geometry.ns` with an unsaved change that removes `Point`, causing cross-file error on `main.ns`,
    // then restore `geometry.ns` and verify `main.ns` diagnostics clear again!
    wire.notify(
        "textDocument/didOpen",
        JsonValue::object([(
            "textDocument",
            JsonValue::object([
                ("uri", JsonValue::from(geom_uri.as_str())),
                ("languageId", JsonValue::from("nessa")),
                ("version", JsonValue::from(1)),
                (
                    "text",
                    JsonValue::from("pub struct OtherBox { pub v: i64 }\n"),
                ),
            ]),
        )]),
    );
    wire.notify(
        "textDocument/didChange",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([
                    ("uri", JsonValue::from(geom_uri.as_str())),
                    ("version", JsonValue::from(2)),
                ]),
            ),
            (
                "contentChanges",
                JsonValue::Array(vec![JsonValue::object([(
                    "text",
                    JsonValue::from("pub struct Point { pub x: i64, pub y: i64 }\n"),
                )])]),
            ),
        ]),
    );
    wire.request(2, "shutdown", JsonValue::Null);
    wire.notify("exit", JsonValue::Null);

    let (code, msgs) = wire.run();
    assert_eq!(code, 0);

    let main_diag_events: Vec<&JsonValue> = msgs
        .iter()
        .filter_map(|m| match m {
            RpcMessage::Notification { method, params }
                if method == "textDocument/publishDiagnostics"
                    && params.get_str("uri") == Some(main_uri.as_str()) =>
            {
                Some(params)
            }
            _ => None,
        })
        .collect();

    assert!(main_diag_events.len() >= 4, "{main_diag_events:?}");
    // 1st event (version 1): type mismatch error on `123`.
    assert!(
        !main_diag_events[0]
            .get_array("diagnostics")
            .unwrap()
            .is_empty()
    );
    // 2nd event (version 2 after UTF-16 edit): clean!
    assert!(
        main_diag_events[1]
            .get_array("diagnostics")
            .unwrap()
            .is_empty(),
        "expected UTF-16 edit after emoji to clear error: {:?}",
        main_diag_events[1]
    );
    // 3rd event (when geometry.ns broke `Point`): error reported on `main.ns`!
    assert!(
        !main_diag_events[2]
            .get_array("diagnostics")
            .unwrap()
            .is_empty()
    );
    // Final event (when geometry.ns restored `Point`): cleared on `main.ns`!
    assert!(
        main_diag_events
            .last()
            .unwrap()
            .get_array("diagnostics")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn hover_navigation_typedef_impl_refs_highlights_and_stdlib_goto_def() {
    let ws = TempWorkspace::new("nav");
    let (geometry_path, effects_path, main_path) = setup_demo_package(&ws);
    let main_text = fs::read_to_string(&main_path).unwrap();
    let geom_text = fs::read_to_string(&geometry_path).unwrap();
    let main_uri = path_to_uri(&main_path);
    let geom_uri = path_to_uri(&geometry_path);
    let effects_uri = path_to_uri(&effects_path);

    let mut wire = RpcWireBuilder::new();
    wire.request(
        1,
        "initialize",
        JsonValue::object([("rootUri", JsonValue::from(path_to_uri(ws.path())))]),
    );
    wire.notify("initialized", JsonValue::empty_object());
    wire.notify(
        "textDocument/didOpen",
        JsonValue::object([(
            "textDocument",
            JsonValue::object([
                ("uri", JsonValue::from(main_uri.as_str())),
                ("languageId", JsonValue::from("nessa")),
                ("version", JsonValue::from(1)),
                ("text", JsonValue::from(main_text.as_str())),
            ]),
        )]),
    );

    // #2: Hover on `load_port` import in `main.ns` -> shows `#log_event !ConfigError i64` + doc comment!
    wire.request(
        2,
        "textDocument/hover",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            ("position", pos_of_nth(&main_text, "load_port", 0).to_json()),
        ]),
    );
    // #3: Hover on local variable `moved` -> shows `let moved: Point`
    wire.request(
        3,
        "textDocument/hover",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            ("position", pos_of_nth(&main_text, "moved", 0).to_json()),
        ]),
    );
    // #4: Hover on `Color` -> shows `pub enum Color` with `red`, `green`, `custom_rgb`
    wire.request(
        4,
        "textDocument/hover",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            ("position", pos_of_nth(&main_text, "Color", 1).to_json()),
        ]),
    );
    // #5: Go-to-Definition on cross-module method call `shifted` -> `src/geometry.ns`
    wire.request(
        5,
        "textDocument/definition",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            ("position", pos_of_nth(&main_text, "shifted", 0).to_json()),
        ]),
    );
    // #6: Go-to-Definition on `load_port` -> `src/effects.ns`
    wire.request(
        6,
        "textDocument/definition",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            ("position", pos_of_nth(&main_text, "load_port", 0).to_json()),
        ]),
    );
    // #7: Go-to-Definition on `println` -> `std` library file (materialized or on-disk `std/`)!
    wire.request(
        7,
        "textDocument/definition",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            ("position", pos_of_nth(&main_text, "println", 0).to_json()),
        ]),
    );
    // #8: Type Definition on `origin` variable -> `pub struct Point` in `src/geometry.ns`!
    wire.request(
        8,
        "textDocument/typeDefinition",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            ("position", pos_of_nth(&main_text, "origin", 1).to_json()),
        ]),
    );
    // #9: Find Implementations on `trait Renderable` in `src/geometry.ns`!
    wire.request(
        9,
        "textDocument/implementation",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(geom_uri.as_str()))]),
            ),
            (
                "position",
                pos_of_nth(&geom_text, "Renderable", 0).to_json(),
            ),
        ]),
    );
    // #10: Find References on `Point` across `geometry.ns` and `main.ns`
    wire.request(
        10,
        "textDocument/references",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(geom_uri.as_str()))]),
            ),
            ("position", pos_of_nth(&geom_text, "Point", 0).to_json()),
            (
                "context",
                JsonValue::object([("includeDeclaration", JsonValue::Bool(true))]),
            ),
        ]),
    );
    // #11: Document Highlights on `counter` in `main.ns` (`Write` on decl & `+=`, `Read` on `+`)
    wire.request(
        11,
        "textDocument/documentHighlight",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            ("position", pos_of_nth(&main_text, "counter", 0).to_json()),
        ]),
    );
    wire.request(12, "shutdown", JsonValue::Null);
    wire.notify("exit", JsonValue::Null);

    let (code, msgs) = wire.run();
    assert_eq!(code, 0);

    let hover_load = find_ok_result(&msgs, 2)
        .get("contents")
        .and_then(|c| c.get_str("value"))
        .unwrap();
    assert!(hover_load.contains("```nessa"), "{hover_load}");
    assert!(
        hover_load.contains("pub fn load_port(key: String) -> #log_event !ConfigError i64"),
        "{hover_load}"
    );
    assert!(hover_load.contains("Loads a server port"), "{hover_load}");

    let hover_moved = find_ok_result(&msgs, 3)
        .get("contents")
        .and_then(|c| c.get_str("value"))
        .unwrap();
    assert!(hover_moved.contains("moved: Point"), "{hover_moved}");

    let hover_color = find_ok_result(&msgs, 4)
        .get("contents")
        .and_then(|c| c.get_str("value"))
        .unwrap();
    assert!(hover_color.contains("pub enum Color"), "{hover_color}");
    assert!(hover_color.contains("custom_rgb"), "{hover_color}");

    let def_shifted = find_ok_result(&msgs, 5).as_array().unwrap();
    assert_eq!(def_shifted[0].get_str("uri"), Some(geom_uri.as_str()));

    let def_load = find_ok_result(&msgs, 6).as_array().unwrap();
    assert_eq!(def_load[0].get_str("uri"), Some(effects_uri.as_str()));

    let def_println = find_ok_result(&msgs, 7).as_array().unwrap();
    let std_target_uri = def_println[0].get_str("uri").unwrap();
    let std_target_path = uri_to_path(std_target_uri).expect("valid std file URI");
    assert!(
        std_target_path.is_file(),
        "expected materialized or library std file on disk: {}",
        std_target_path.display()
    );
    assert!(
        std_target_uri.contains("std/"),
        "expected std path in URI, got {std_target_uri}"
    );

    let typedef_origin = find_ok_result(&msgs, 8).as_array().unwrap();
    assert_eq!(typedef_origin[0].get_str("uri"), Some(geom_uri.as_str()));

    let impls = find_ok_result(&msgs, 9).as_array().unwrap();
    assert!(!impls.is_empty(), "expected Renderable implementation");
    assert_eq!(impls[0].get_str("uri"), Some(geom_uri.as_str()));

    let refs = find_ok_result(&msgs, 10).as_array().unwrap();
    let ref_uris: Vec<&str> = refs.iter().filter_map(|r| r.get_str("uri")).collect();
    assert!(ref_uris.contains(&geom_uri.as_str()));
    assert!(ref_uris.contains(&main_uri.as_str()));
    assert!(
        refs.len() >= 5,
        "expected multiple Point references: {refs:?}"
    );

    let highlights = find_ok_result(&msgs, 11).as_array().unwrap();
    assert_eq!(highlights.len(), 3, "{highlights:?}");
    let kinds: Vec<u32> = highlights
        .iter()
        .filter_map(|h| h.get_u32("kind"))
        .collect();
    // decl (Write=3), `counter += 5` (Write=3), `counter + moved.y` (Read=2)
    assert_eq!(kinds, vec![3, 3, 2]);
}

#[test]
fn syntax_broken_buffer_resilience_and_all_completion_modes() {
    let ws = TempWorkspace::new("completion");
    let (_, _, main_path) = setup_demo_package(&ws);
    let main_uri = path_to_uri(&main_path);

    // Open clean first to populate `last_good`, then replace with a syntax-broken mid-typing buffer!
    let clean_main = fs::read_to_string(&main_path).unwrap();
    let broken_main = r#"--% .
use std.
use @

use @geometry.{Color, Point}

fn broken_helper() {
    let unfinished =
}

fn active_edit() {
    let p = Point { x: 10, y: 20 }
    p.
    Color.
    p'
}

fn still_healthy(pt: Point) -> i64 {
    pt.x + pt.y
}
"#;

    let mut wire = RpcWireBuilder::new();
    wire.request(
        1,
        "initialize",
        JsonValue::object([("rootUri", JsonValue::from(path_to_uri(ws.path())))]),
    );
    wire.notify("initialized", JsonValue::empty_object());
    wire.notify(
        "textDocument/didOpen",
        JsonValue::object([(
            "textDocument",
            JsonValue::object([
                ("uri", JsonValue::from(main_uri.as_str())),
                ("languageId", JsonValue::from("nessa")),
                ("version", JsonValue::from(1)),
                ("text", JsonValue::from(clean_main.as_str())),
            ]),
        )]),
    );
    wire.notify(
        "textDocument/didChange",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([
                    ("uri", JsonValue::from(main_uri.as_str())),
                    ("version", JsonValue::from(2)),
                ]),
            ),
            (
                "contentChanges",
                JsonValue::Array(vec![JsonValue::object([(
                    "text",
                    JsonValue::from(broken_main),
                )])]),
            ),
        ]),
    );

    // #2: `--% .` directive completion on line 0
    wire.request(
        2,
        "textDocument/completion",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            ("position", pos_after(broken_main, "--% .").to_json()),
        ]),
    );
    // #3: `use std.` module completion
    wire.request(
        3,
        "textDocument/completion",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            ("position", pos_after(broken_main, "use std.").to_json()),
        ]),
    );
    // #4: `use @` current-package module completion
    wire.request(
        4,
        "textDocument/completion",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            ("position", pos_after(broken_main, "use @").to_json()),
        ]),
    );
    // #5: Mid-keystroke `p.` struct fields & methods completion despite broken `broken_helper` above!
    wire.request(
        5,
        "textDocument/completion",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            ("position", pos_after(broken_main, "    p.").to_json()),
        ]),
    );
    // #6: `Color.` enum variant completion (`red`, `green`, `custom_rgb`)
    wire.request(
        6,
        "textDocument/completion",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            ("position", pos_after(broken_main, "    Color.").to_json()),
        ]),
    );
    // #7: `p'` view completion (`type`)
    wire.request(
        7,
        "textDocument/completion",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            ("position", pos_after(broken_main, "    p'").to_json()),
        ]),
    );
    // #8: Hover on `still_healthy` below broken code still works!
    wire.request(
        8,
        "textDocument/hover",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            (
                "position",
                pos_of_nth(broken_main, "still_healthy", 0).to_json(),
            ),
        ]),
    );
    wire.request(9, "shutdown", JsonValue::Null);
    wire.notify("exit", JsonValue::Null);

    let (code, msgs) = wire.run();
    assert_eq!(code, 0);

    let labels_of = |req_id: i64| -> Vec<String> {
        find_ok_result(&msgs, req_id)
            .get_array("items")
            .unwrap()
            .iter()
            .filter_map(|i| i.get_str("label").map(str::to_owned))
            .collect()
    };

    let dir_labels = labels_of(2);
    for expected in [
        ".skip",
        ".preserve",
        ".off",
        ".on",
        ".max_width",
        ".tab_spaces",
        ".newline_style",
        ".use_small_heuristics",
        ".reorder_imports",
        ".normalize_comments",
    ] {
        assert!(
            dir_labels.contains(&expected.to_owned()),
            "missing {expected} in {dir_labels:?}"
        );
    }

    let std_labels = labels_of(3);
    for expected in ["io", "fs", "collections", "string", "json", "math"] {
        assert!(
            std_labels.contains(&expected.to_owned()),
            "missing std.{expected} in {std_labels:?}"
        );
    }

    let at_labels = labels_of(4);
    assert!(at_labels.contains(&"geometry".to_owned()), "{at_labels:?}");
    assert!(at_labels.contains(&"effects".to_owned()), "{at_labels:?}");

    let p_labels = labels_of(5);
    for expected in ["x", "y", "shifted", "render_label"] {
        assert!(
            p_labels.contains(&expected.to_owned()),
            "missing p.{expected} in {p_labels:?}"
        );
    }

    let color_labels = labels_of(6);
    for expected in ["red", "green", "custom_rgb"] {
        assert!(
            color_labels.contains(&expected.to_owned()),
            "missing Color.{expected} in {color_labels:?}"
        );
    }

    let view_labels = labels_of(7);
    assert!(view_labels.contains(&"type".to_owned()), "{view_labels:?}");

    let hover_healthy = find_ok_result(&msgs, 8)
        .get("contents")
        .and_then(|c| c.get_str("value"))
        .unwrap();
    assert!(
        hover_healthy.contains("fn still_healthy(pt: Point) -> i64"),
        "{hover_healthy}"
    );
}

#[test]
fn prepare_rename_multifile_rename_backticks_and_signature_help() {
    let ws = TempWorkspace::new("rename-sig");
    let (geometry_path, _, main_path) = setup_demo_package(&ws);
    let geom_uri = path_to_uri(&geometry_path);
    let main_uri = path_to_uri(&main_path);

    let sig_main = r#"use std.io
use @effects.configure_server
use @geometry.Point

fn main() {
    let `custom var` = 10
    let sum = `custom var` + 1
    let p = Point { x: 1, y: 2 }
    let q = p.shifted(5, )
    let s = configure_server(timeout_ms = 1000, host = "local")
    println(s)
}
"#;

    let mut wire = RpcWireBuilder::new();
    wire.request(
        1,
        "initialize",
        JsonValue::object([("rootUri", JsonValue::from(path_to_uri(ws.path())))]),
    );
    wire.notify("initialized", JsonValue::empty_object());
    wire.notify(
        "textDocument/didOpen",
        JsonValue::object([(
            "textDocument",
            JsonValue::object([
                ("uri", JsonValue::from(main_uri.as_str())),
                ("languageId", JsonValue::from("nessa")),
                ("version", JsonValue::from(1)),
                ("text", JsonValue::from(sig_main)),
            ]),
        )]),
    );

    // #2: prepareRename on `println` (external std symbol) -> null!
    wire.request(
        2,
        "textDocument/prepareRename",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            ("position", pos_of_nth(sig_main, "println", 0).to_json()),
        ]),
    );
    // #3: prepareRename on backtick identifier `` `custom var` `` -> placeholder "custom var"
    wire.request(
        3,
        "textDocument/prepareRename",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            (
                "position",
                pos_of_nth(sig_main, "`custom var`", 0).to_json(),
            ),
        ]),
    );
    // #4: rename `` `custom var` `` to keyword `"match"` -> wraps as `` `match` `` across both occurrences!
    wire.request(
        4,
        "textDocument/rename",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            (
                "position",
                pos_of_nth(sig_main, "`custom var`", 0).to_json(),
            ),
            ("newName", JsonValue::from("match")),
        ]),
    );
    // #5: multi-file rename of `shifted` -> `translated` across `geometry.ns` and `main.ns`!
    wire.request(
        5,
        "textDocument/rename",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            ("position", pos_of_nth(sig_main, "shifted", 0).to_json()),
            ("newName", JsonValue::from("translated")),
        ]),
    );
    // #6: Signature Help on positional 2nd arg `p.shifted(5, |)` -> activeParameter == 1 (`.dy: i64 = 0`)
    wire.request(
        6,
        "textDocument/signatureHelp",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            ("position", pos_after(sig_main, "p.shifted(5, ").to_json()),
        ]),
    );
    // #7: Signature Help on named argument `configure_server(timeout_ms = |` -> activeParameter == 1!
    wire.request(
        7,
        "textDocument/signatureHelp",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            (
                "position",
                pos_after(sig_main, "configure_server(timeout_ms = ").to_json(),
            ),
        ]),
    );
    wire.request(8, "shutdown", JsonValue::Null);
    wire.notify("exit", JsonValue::Null);

    let (code, msgs) = wire.run();
    assert_eq!(code, 0);

    assert!(find_ok_result(&msgs, 2).is_null());

    let prep_bt = find_ok_result(&msgs, 3);
    assert_eq!(prep_bt.get_str("placeholder"), Some("custom var"));

    let rename_bt = find_ok_result(&msgs, 4);
    let bt_edits = rename_bt
        .get("changes")
        .and_then(|c| c.get_array(&main_uri))
        .unwrap();
    assert_eq!(bt_edits.len(), 2);
    assert_eq!(bt_edits[0].get_str("newText"), Some("`match`"));

    let rename_multi = find_ok_result(&msgs, 5).get("changes").unwrap();
    assert!(
        rename_multi
            .get_array(&geom_uri)
            .is_some_and(|e| e.len() == 1)
    );
    assert!(
        rename_multi
            .get_array(&main_uri)
            .is_some_and(|e| e.len() == 1)
    );

    let sh_pos = find_ok_result(&msgs, 6);
    assert_eq!(sh_pos.get_u32("activeParameter"), Some(1));
    let sh_params = sh_pos.get_array("signatures").unwrap()[0]
        .get_array("parameters")
        .unwrap();
    assert_eq!(
        sh_params.len(),
        2,
        "self must be omitted for instance call: {sh_params:?}"
    );
    assert!(
        sh_params[1]
            .get_str("label")
            .unwrap()
            .contains(".dy: i64 = 0")
    );

    let sh_named = find_ok_result(&msgs, 7);
    assert_eq!(
        sh_named.get_u32("activeParameter"),
        Some(1),
        "named arg `timeout_ms` should highlight parameter index 1: {sh_named:?}"
    );
}

fn custom_doc_formatter(_path: &Path, src: &str, tab_spaces: u32) -> Result<String, String> {
    Ok(format!(
        "-- formatted(tab={tab_spaces})\n{}",
        src.trim_end()
    ))
}

fn custom_range_formatter(
    _path: &Path,
    src: &str,
    range: (u32, u32),
    tab_spaces: u32,
) -> Result<String, String> {
    Ok(format!(
        "-- range({}..{},tab={tab_spaces})\n{src}",
        range.0, range.1
    ))
}

#[test]
fn symbols_semantic_tokens_folding_formatting_hooks_and_cli_check() {
    let ws = TempWorkspace::new("symbols-fmt-cli");
    let (geometry_path, _, _) = setup_demo_package(&ws);
    let geom_uri = path_to_uri(&geometry_path);

    let foldable_src = r#"-- Header line 1
-- Header line 2
use std.io
use std.fs

{-
  Multi-line comment block
-}
--% .off
fn unformatted()   {   }
--% .on

pub struct Widget {
    pub width: i64,
    pub height: i64,
}
"#;
    let foldable_path = ws.write("src/foldable.ns", foldable_src);
    let foldable_uri = path_to_uri(&foldable_path);

    let cfg = ServerConfig::default()
        .with_formatter(Some(custom_doc_formatter), Some(custom_range_formatter));

    let mut wire = RpcWireBuilder::new();
    wire.request(
        1,
        "initialize",
        JsonValue::object([("rootUri", JsonValue::from(path_to_uri(ws.path())))]),
    );
    wire.notify("initialized", JsonValue::empty_object());
    wire.notify(
        "textDocument/didOpen",
        JsonValue::object([(
            "textDocument",
            JsonValue::object([
                ("uri", JsonValue::from(foldable_uri.as_str())),
                ("languageId", JsonValue::from("nessa")),
                ("version", JsonValue::from(1)),
                ("text", JsonValue::from(foldable_src)),
            ]),
        )]),
    );

    // #2: Document Symbols on `geometry.ns`
    wire.request(
        2,
        "textDocument/documentSymbol",
        JsonValue::object([(
            "textDocument",
            JsonValue::object([("uri", JsonValue::from(geom_uri.as_str()))]),
        )]),
    );
    // #3: Workspace Symbols fuzzy query `"render_lbl"`
    wire.request(
        3,
        "workspace/symbol",
        JsonValue::object([("query", JsonValue::from("render_lbl"))]),
    );
    // #4: Semantic Tokens on `foldable.ns`
    wire.request(
        4,
        "textDocument/semanticTokens/full",
        JsonValue::object([(
            "textDocument",
            JsonValue::object([("uri", JsonValue::from(foldable_uri.as_str()))]),
        )]),
    );
    // #5: Folding Ranges on `foldable.ns`
    wire.request(
        5,
        "textDocument/foldingRange",
        JsonValue::object([(
            "textDocument",
            JsonValue::object([("uri", JsonValue::from(foldable_uri.as_str()))]),
        )]),
    );
    // #6: Full Document Formatting via custom hook
    wire.request(
        6,
        "textDocument/formatting",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(foldable_uri.as_str()))]),
            ),
            (
                "options",
                JsonValue::object([("tabSize", JsonValue::from(4u32))]),
            ),
        ]),
    );
    // #7: Range Formatting via custom hook
    wire.request(
        7,
        "textDocument/rangeFormatting",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(foldable_uri.as_str()))]),
            ),
            (
                "range",
                LspRange::new(LspPosition::new(0, 0), LspPosition::new(1, 5)).to_json(),
            ),
            (
                "options",
                JsonValue::object([("tabSize", JsonValue::from(2u32))]),
            ),
        ]),
    );
    wire.request(8, "shutdown", JsonValue::Null);
    wire.notify("exit", JsonValue::Null);

    let (code, msgs) = wire.run_with_config(cfg);
    assert_eq!(code, 0);

    let doc_syms = find_ok_result(&msgs, 2).as_array().unwrap();
    let point_sym = doc_syms
        .iter()
        .find(|s| s.get_str("name") == Some("Point"))
        .expect("Point outline symbol");
    let point_children = point_sym.get_array("children").unwrap();
    assert_eq!(point_children.len(), 2);

    let ws_syms = find_ok_result(&msgs, 3).as_array().unwrap();
    assert!(
        ws_syms
            .iter()
            .any(|s| s.get_str("name") == Some("render_label")),
        "expected fuzzy match `render_label` in {ws_syms:?}"
    );

    let sem_data = find_ok_result(&msgs, 4).get_array("data").unwrap();
    assert!(!sem_data.is_empty());
    assert_eq!(sem_data.len() % 5, 0);

    let folds = find_ok_result(&msgs, 5).as_array().unwrap();
    let fold_kinds: Vec<&str> = folds.iter().filter_map(|f| f.get_str("kind")).collect();
    assert!(fold_kinds.contains(&"comment"), "{folds:?}");
    assert!(fold_kinds.contains(&"imports"), "{folds:?}");
    assert!(fold_kinds.contains(&"region"), "{folds:?}");

    let fmt_edits = find_ok_result(&msgs, 6).as_array().unwrap();
    assert_eq!(fmt_edits.len(), 1);
    assert!(
        fmt_edits[0]
            .get_str("newText")
            .unwrap()
            .starts_with("-- formatted(tab=4)\n")
    );

    let rfmt_edits = find_ok_result(&msgs, 7).as_array().unwrap();
    assert_eq!(rfmt_edits.len(), 1);
    assert!(rfmt_edits[0].get_str("newText").unwrap().contains("tab=2)"));

    // Verify `nessa-lsp --check` CLI behavior on clean package, broken file, help, and unknown flag!
    assert_eq!(
        run_cli_with_config(
            ["--check", ws.path().to_str().unwrap()],
            ServerConfig::default(),
        ),
        0
    );

    let bad_script = ws.write("broken_standalone.ns", "fn main() { let x: bool = 99 }\n");
    assert_eq!(
        run_cli_with_config(
            ["--check", bad_script.to_str().unwrap()],
            ServerConfig::default(),
        ),
        1
    );
    assert_eq!(
        run_cli_with_config(["--version"], ServerConfig::default()),
        0
    );
    assert_eq!(
        run_cli_with_config(["--bogus-flag"], ServerConfig::default()),
        2
    );
}

#[test]
fn standalone_script_bundled_regex_goto_definition_works() {
    let ws = TempWorkspace::new("standalone-regex");
    let script_src = r#"use regex.Regex

fn main() {
    let pat = Regex.compile("[0-9]+")
}
"#;
    let script_path = ws.write("standalone_regex.ns", script_src);
    let script_uri = path_to_uri(&script_path);

    let mut wire = RpcWireBuilder::new();
    wire.request(
        1,
        "initialize",
        JsonValue::object([("rootUri", JsonValue::from(path_to_uri(ws.path())))]),
    );
    wire.notify("initialized", JsonValue::empty_object());
    wire.notify(
        "textDocument/didOpen",
        JsonValue::object([(
            "textDocument",
            JsonValue::object([
                ("uri", JsonValue::from(script_uri.as_str())),
                ("languageId", JsonValue::from("nessa")),
                ("version", JsonValue::from(1)),
                ("text", JsonValue::from(script_src)),
            ]),
        )]),
    );
    wire.request(
        2,
        "textDocument/definition",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(script_uri.as_str()))]),
            ),
            ("position", pos_of_nth(script_src, "compile", 0).to_json()),
        ]),
    );
    wire.request(3, "shutdown", JsonValue::Null);
    wire.notify("exit", JsonValue::Null);

    let (code, msgs) = wire.run();
    assert_eq!(code, 0);

    let defs = find_ok_result(&msgs, 2).as_array().unwrap();
    assert!(!defs.is_empty());
    let target_uri = defs[0].get_str("uri").unwrap();
    assert!(target_uri.contains("regex/src/lib.ns"), "{target_uri}");
    let target_path = uri_to_path(target_uri).unwrap();
    assert!(target_path.is_file());
}

#[test]
fn path_dependency_live_overlay_pipeline_spans_async_effect_and_privileged_views() {
    let ws = TempWorkspace::new("dep-pipeline-priv");

    ws.write(
        "helper_lib/package.toml",
        r#"[package]
name = "helper_lib"
domain = "test.org"
version = "0.1.0"
type = "lib"
"#,
    );
    let dep_lib_src = r#"--- Multiplies input value by factor.
pub fn scale_value(val: i64, factor: i64) -> i64 = val * factor
"#;
    let dep_lib_path = ws.write("helper_lib/src/lib.ns", dep_lib_src);
    let dep_lib_uri = path_to_uri(&dep_lib_path);

    ws.write(
        "app_pkg/package.toml",
        r#"[package]
name = "app_pkg"
domain = "test.org"
version = "0.1.0"
type = "exe"

[dependencies]
helper_lib = { path = "../helper_lib" }
"#,
    );
    let app_main_src = r#"use helper_lib.scale_value

pub async effect fetch_remote(endpoint: String) -> String

fn main() {
    let seed = 7
    let piped = seed |> scale_value(3)
    let view_probe = .print'
    let _ = piped
}
"#;
    let app_main_path = ws.write("app_pkg/src/main.ns", app_main_src);
    let app_main_uri = path_to_uri(&app_main_path);

    let mut wire = RpcWireBuilder::new();
    wire.request(
        1,
        "initialize",
        JsonValue::object([
            (
                "rootUri",
                JsonValue::from(path_to_uri(&ws.path().join("app_pkg"))),
            ),
            (
                "initializationOptions",
                JsonValue::object([(
                    "privilegedPackages",
                    JsonValue::Array(vec![JsonValue::from("test.org/app_pkg")]),
                )]),
            ),
        ]),
    );
    wire.notify("initialized", JsonValue::empty_object());
    // Open `helper_lib/src/lib.ns` with an unsaved new function `bonus_calc`, and open `app_pkg/src/main.ns`!
    let updated_dep_src = r#"--- Multiplies input value by factor.
pub fn scale_value(val: i64, factor: i64) -> i64 = val * factor

pub fn unsaved_dep_fn(x: i64) -> i64 = x + 100
"#;
    wire.notify(
        "textDocument/didOpen",
        JsonValue::object([(
            "textDocument",
            JsonValue::object([
                ("uri", JsonValue::from(dep_lib_uri.as_str())),
                ("languageId", JsonValue::from("nessa")),
                ("version", JsonValue::from(1)),
                ("text", JsonValue::from(updated_dep_src)),
            ]),
        )]),
    );
    wire.notify(
        "textDocument/didOpen",
        JsonValue::object([(
            "textDocument",
            JsonValue::object([
                ("uri", JsonValue::from(app_main_uri.as_str())),
                ("languageId", JsonValue::from("nessa")),
                ("version", JsonValue::from(1)),
                ("text", JsonValue::from(app_main_src)),
            ]),
        )]),
    );

    // #2: Go-to-Definition on `scale_value` inside pipeline `seed |> scale_value(3)` -> jumps to `helper_lib/src/lib.ns`!
    wire.request(
        2,
        "textDocument/definition",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(app_main_uri.as_str()))]),
            ),
            (
                "position",
                pos_of_nth(app_main_src, "scale_value", 1).to_json(),
            ),
        ]),
    );
    // #3: Hover on `seed` LHS of pipeline `seed |> scale_value(3)` -> resolves to `seed: i64` (not synthetic Call)!
    wire.request(
        3,
        "textDocument/hover",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(app_main_uri.as_str()))]),
            ),
            ("position", pos_of_nth(app_main_src, "seed", 1).to_json()),
        ]),
    );
    // #4: Hover on `fetch_remote` -> shows `pub async effect fetch_remote(endpoint: String) -> String`!
    wire.request(
        4,
        "textDocument/hover",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(app_main_uri.as_str()))]),
            ),
            (
                "position",
                pos_of_nth(app_main_src, "fetch_remote", 0).to_json(),
            ),
        ]),
    );
    // #5: `'` view completion on `.print'` inside privileged `test.org/app_pkg` -> includes `"builtin"` and `"type"`!
    wire.request(
        5,
        "textDocument/completion",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(app_main_uri.as_str()))]),
            ),
            ("position", pos_after(app_main_src, ".print'").to_json()),
        ]),
    );
    wire.request(6, "shutdown", JsonValue::Null);
    wire.notify("exit", JsonValue::Null);

    let (code, msgs) = wire.run();
    assert_eq!(code, 0);

    let def_scale = find_ok_result(&msgs, 2).as_array().unwrap();
    assert_eq!(def_scale[0].get_str("uri"), Some(dep_lib_uri.as_str()));

    let hover_seed = find_ok_result(&msgs, 3)
        .get("contents")
        .and_then(|c| c.get_str("value"))
        .unwrap();
    assert!(hover_seed.contains("seed: i64"), "{hover_seed}");

    let hover_async_eff = find_ok_result(&msgs, 4)
        .get("contents")
        .and_then(|c| c.get_str("value"))
        .unwrap();
    assert!(
        hover_async_eff.contains("pub async effect fetch_remote(endpoint: String) -> String"),
        "{hover_async_eff}"
    );

    let view_items: Vec<&str> = find_ok_result(&msgs, 5)
        .get_array("items")
        .unwrap()
        .iter()
        .filter_map(|i| i.get_str("label"))
        .collect();
    assert!(view_items.contains(&"builtin"), "{view_items:?}");
    assert!(view_items.contains(&"type"), "{view_items:?}");
}

#[test]
fn default_nessa_fmt_document_and_range_formatting_respects_directives() {
    let ws = TempWorkspace::new("real-nessa-fmt");
    let raw_src = "\
use @local_mod.Item
use std.io.println

fn first_unformatted(a:i64,b:i64)->i64=a+b

--% .skip
fn skipped_layout( x : i64 ) -> i64 = x+1

fn second_unformatted(x:i64)->i64{
let doubled=x*2
doubled
}
";
    let file_path = ws.write("sample_fmt.ns", raw_src);
    let file_uri = path_to_uri(&file_path);

    let mut wire = RpcWireBuilder::new();
    wire.request(
        1,
        "initialize",
        JsonValue::object([("rootUri", JsonValue::from(path_to_uri(ws.path())))]),
    );
    wire.notify("initialized", JsonValue::empty_object());
    wire.notify(
        "textDocument/didOpen",
        JsonValue::object([(
            "textDocument",
            JsonValue::object([
                ("uri", JsonValue::from(file_uri.as_str())),
                ("languageId", JsonValue::from("nessa")),
                ("version", JsonValue::from(1)),
                ("text", JsonValue::from(raw_src)),
            ]),
        )]),
    );
    // #2: Range formatting ONLY `second_unformatted` (lines 8..11) — keeps imports & `first_unformatted` untouched!
    wire.request(
        2,
        "textDocument/rangeFormatting",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(file_uri.as_str()))]),
            ),
            (
                "range",
                LspRange::new(LspPosition::new(8, 0), LspPosition::new(11, 1)).to_json(),
            ),
            (
                "options",
                JsonValue::object([("tabSize", JsonValue::from(4u32))]),
            ),
        ]),
    );
    // #3: Full document formatting — reorders `std` before `@local_mod`, formats both functions, keeps `--% .skip`!
    wire.request(
        3,
        "textDocument/formatting",
        JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(file_uri.as_str()))]),
            ),
            (
                "options",
                JsonValue::object([("tabSize", JsonValue::from(4u32))]),
            ),
        ]),
    );
    wire.request(4, "shutdown", JsonValue::Null);
    wire.notify("exit", JsonValue::Null);

    let (code, msgs) = wire.run();
    assert_eq!(code, 0);

    let range_edits = find_ok_result(&msgs, 2).as_array().unwrap();
    assert_eq!(range_edits.len(), 1);
    let range_text = range_edits[0].get_str("newText").unwrap();
    assert!(range_text.starts_with("use @local_mod.Item\nuse std.io.println\n"));
    assert!(range_text.contains("fn first_unformatted(a:i64,b:i64)->i64=a+b"));
    assert!(range_text.contains(
        "fn second_unformatted(x: i64) -> i64 {\n    let doubled = x * 2\n    doubled\n}"
    ));

    let full_edits = find_ok_result(&msgs, 3).as_array().unwrap();
    assert_eq!(full_edits.len(), 1);
    let full_text = full_edits[0].get_str("newText").unwrap();
    assert!(full_text.starts_with("use std.io.println\n\nuse @local_mod.Item\n"));
    assert!(full_text.contains("fn first_unformatted(a: i64, b: i64) -> i64 = a + b"));
    assert!(full_text.contains("--% .skip\nfn skipped_layout( x : i64 ) -> i64 = x+1"));
    assert!(full_text.contains(
        "fn second_unformatted(x: i64) -> i64 {\n    let doubled = x * 2\n    doubled\n}"
    ));
}

#[test]
fn coalesced_stdio_burst_debounces_did_change_and_serves_instant_dot_completion() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let ws = TempWorkspace::new("coalesced-burst");
    let (_, _, main_path) = setup_demo_package(&ws);
    let main_uri = path_to_uri(&main_path);
    let initial = "use @geometry.Point\nfn main() {\n    let pt = Point { x: 1, y: 2 }\n    let v = pt.x\n}\n";

    let (tx, rx) = mpsc::channel();
    tx.send(Ok(RpcMessage::Request {
        id: RequestId::Int(1),
        method: "initialize".to_owned(),
        params: JsonValue::object([("rootUri", JsonValue::from(path_to_uri(ws.path())))]),
    }))
    .unwrap();
    tx.send(Ok(RpcMessage::Notification {
        method: "initialized".to_owned(),
        params: JsonValue::empty_object(),
    }))
    .unwrap();
    tx.send(Ok(RpcMessage::Notification {
        method: "textDocument/didOpen".to_owned(),
        params: JsonValue::object([(
            "textDocument",
            JsonValue::object([
                ("uri", JsonValue::from(main_uri.as_str())),
                ("languageId", JsonValue::from("nessa")),
                ("version", JsonValue::from(1)),
                ("text", JsonValue::from(initial)),
            ]),
        )]),
    }))
    .unwrap();

    // Queue 15 rapid `didChange` notifications in a single burst ending with `pt.`!
    for ver in 2..=16i64 {
        let edited = if ver < 16 {
            format!(
                "use @geometry.Point\nfn main() {{\n    let pt = Point {{ x: {ver}, y: 2 }}\n    let v = pt.x\n}}\n"
            )
        } else {
            "use @geometry.Point\nfn main() {\n    let pt = Point { x: 16, y: 2 }\n    pt.\n}\n"
                .to_owned()
        };
        tx.send(Ok(RpcMessage::Notification {
            method: "textDocument/didChange".to_owned(),
            params: JsonValue::object([
                (
                    "textDocument",
                    JsonValue::object([
                        ("uri", JsonValue::from(main_uri.as_str())),
                        ("version", JsonValue::from(ver)),
                    ]),
                ),
                (
                    "contentChanges",
                    JsonValue::Array(vec![JsonValue::object([(
                        "text",
                        JsonValue::from(edited.as_str()),
                    )])]),
                ),
            ]),
        }))
        .unwrap();
    }

    // Queue a cancelled request (#2) + active dot-completion request (#3) right after `pt.`!
    tx.send(Ok(RpcMessage::Request {
        id: RequestId::Int(2),
        method: "textDocument/hover".to_owned(),
        params: JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            ("position", LspPosition::new(2, 8).to_json()),
        ]),
    }))
    .unwrap();
    tx.send(Ok(RpcMessage::Notification {
        method: "$/cancelRequest".to_owned(),
        params: JsonValue::object([("id", JsonValue::from(2))]),
    }))
    .unwrap();
    tx.send(Ok(RpcMessage::Request {
        id: RequestId::Int(3),
        method: "textDocument/completion".to_owned(),
        params: JsonValue::object([
            (
                "textDocument",
                JsonValue::object([("uri", JsonValue::from(main_uri.as_str()))]),
            ),
            ("position", LspPosition::new(3, 7).to_json()),
        ]),
    }))
    .unwrap();
    drop(tx);

    let mut server = LanguageServer::default();
    let mut output = Vec::new();
    let t0 = Instant::now();
    let code = server
        .run_coalesced_channel(rx, &mut output, Duration::from_millis(5))
        .expect("coalesced channel run");
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(code, 0);

    let mut out_reader = Cursor::new(output);
    let mut msgs = Vec::new();
    while let Some(msg) = read_rpc_message(&mut out_reader).unwrap() {
        msgs.push(msg);
    }

    // Cancelled request #2 must return -32800 without running hover flush!
    match find_response(&msgs, 2) {
        RpcMessage::Response {
            error: Some(err), ..
        } => assert_eq!(err.code, -32800),
        other => panic!("expected -32800 for cancelled request #2, got {other:?}"),
    }

    // Dot completion #3 on `pt.` must resolve `x` and `y` from `last_good_snapshots`!
    let comp_items = find_ok_result(&msgs, 3)
        .get("items")
        .and_then(JsonValue::as_array)
        .expect("completion items");
    let labels: Vec<&str> = comp_items
        .iter()
        .filter_map(|i| i.get_str("label"))
        .collect();
    assert!(
        labels.contains(&"x") && labels.contains(&"y"),
        "expected Point fields x & y in {labels:?} (elapsed={elapsed_ms:.1}ms)"
    );

    // 15 `didChange` notifications must have coalesced into ONLY 2 diagnostic publications total
    // (1 for `didOpen` v1 + 1 final flush for `didChange` v16)!
    let main_diag_versions: Vec<i64> = msgs
        .iter()
        .filter_map(|m| match m {
            RpcMessage::Notification { method, params }
                if method == "textDocument/publishDiagnostics"
                    && params.get_str("uri") == Some(main_uri.as_str()) =>
            {
                params.get_i64("version")
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        main_diag_versions,
        vec![1, 16],
        "expected 15 didChange events to coalesce into a single v16 diagnostic flush"
    );
}
