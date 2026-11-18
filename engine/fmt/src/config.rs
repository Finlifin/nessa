//! Configuration model, `nessafmt.toml` parser, CLI override parser, and `ignore` matching.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

pub const CONFIG_FILE_NAME: &str = "nessafmt.toml";

/// Line-ending policy for formatted output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NewlineStyle {
    #[default]
    Lf,
    CrLf,
    Auto,
}

impl NewlineStyle {
    pub fn as_toml_str(self) -> &'static str {
        match self {
            Self::Lf => "lf",
            Self::CrLf => "crlf",
            Self::Auto => "auto",
        }
    }

    pub fn as_symbol_str(self) -> &'static str {
        match self {
            Self::Lf => ".lf",
            Self::CrLf => ".crlf",
            Self::Auto => ".auto",
        }
    }

    pub fn parse_value(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        let unquoted = strip_matching_quotes(trimmed).unwrap_or(trimmed);
        let stripped = unquoted.trim().strip_prefix('.').unwrap_or(unquoted.trim());
        match stripped {
            "lf" | "Lf" | "LF" | "unix" | "Unix" => Some(Self::Lf),
            "crlf" | "CrLf" | "CRLF" | "windows" | "Windows" => Some(Self::CrLf),
            "auto" | "Auto" | "AUTO" | "native" | "Native" => Some(Self::Auto),
            _ => None,
        }
    }

    /// Resolve the concrete line separator to emit for a given input buffer.
    pub fn resolve(self, original_source: &str) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::Auto => {
                if original_source.contains("\r\n") {
                    "\r\n"
                } else {
                    "\n"
                }
            }
        }
    }
}

/// Sub-element single-line width heuristics mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SmallHeuristics {
    #[default]
    Default,
    Off,
    Max,
}

impl SmallHeuristics {
    pub fn as_toml_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Off => "off",
            Self::Max => "max",
        }
    }

    pub fn as_symbol_str(self) -> &'static str {
        match self {
            Self::Default => ".default",
            Self::Off => ".off",
            Self::Max => ".max",
        }
    }

    pub fn parse_value(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        let unquoted = strip_matching_quotes(trimmed).unwrap_or(trimmed);
        let stripped = unquoted.trim().strip_prefix('.').unwrap_or(unquoted.trim());
        match stripped {
            "default" | "Default" | "DEFAULT" => Some(Self::Default),
            "off" | "Off" | "OFF" => Some(Self::Off),
            "max" | "Max" | "MAX" => Some(Self::Max),
            _ => None,
        }
    }
}

/// Concrete sub-budgets derived from `max_width` and `use_small_heuristics`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WidthHeuristics {
    pub fn_call_width: usize,
    pub struct_lit_width: usize,
    pub struct_variant_width: usize,
    pub array_width: usize,
    pub chain_width: usize,
    pub single_line_if_else_max_width: usize,
    pub single_line_let_else_max_width: usize,
}

impl WidthHeuristics {
    pub fn from_config(max_width: usize, mode: SmallHeuristics) -> Self {
        match mode {
            SmallHeuristics::Off => Self {
                fn_call_width: 0,
                struct_lit_width: 0,
                struct_variant_width: 0,
                array_width: 0,
                chain_width: 0,
                single_line_if_else_max_width: 0,
                single_line_let_else_max_width: 0,
            },
            SmallHeuristics::Max => Self {
                fn_call_width: max_width,
                struct_lit_width: max_width,
                struct_variant_width: max_width,
                array_width: max_width,
                chain_width: max_width,
                single_line_if_else_max_width: max_width,
                single_line_let_else_max_width: max_width,
            },
            SmallHeuristics::Default => Self {
                fn_call_width: (max_width * 60 / 100).max(1),
                struct_lit_width: (max_width * 35 / 100).max(1),
                struct_variant_width: (max_width * 35 / 100).max(1),
                array_width: (max_width * 60 / 100).max(1),
                chain_width: (max_width * 60 / 100).max(1),
                single_line_if_else_max_width: (max_width * 50 / 100).max(1),
                single_line_let_else_max_width: (max_width * 50 / 100).max(1),
            },
        }
    }
}

/// Configuration error (invalid `nessafmt.toml` or `--config` / `--config-path`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub message: String,
}

impl ConfigError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConfigError {}

/// Complete Nessa formatter configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatConfig {
    pub max_width: usize,
    pub tab_spaces: usize,
    pub newline_style: NewlineStyle,
    pub use_small_heuristics: SmallHeuristics,
    pub reorder_imports: bool,
    pub normalize_comments: bool,
    pub ignore: Vec<String>,
    /// Base directory of the loaded `nessafmt.toml` (if any), used for `ignore` resolution.
    pub config_dir: Option<PathBuf>,
}

impl Default for FormatConfig {
    fn default() -> Self {
        Self {
            max_width: 100,
            tab_spaces: 4,
            newline_style: NewlineStyle::Lf,
            use_small_heuristics: SmallHeuristics::Default,
            reorder_imports: true,
            normalize_comments: true,
            ignore: Vec::new(),
            config_dir: None,
        }
    }
}

impl FormatConfig {
    pub fn width_heuristics(&self) -> WidthHeuristics {
        WidthHeuristics::from_config(self.max_width, self.use_small_heuristics)
    }

    /// Discover `nessafmt.toml` upward from `start_dir` (or load `explicit_config_path`).
    pub fn discover(
        start_dir: &Path,
        explicit_config_path: Option<&Path>,
    ) -> Result<Self, ConfigError> {
        let mut config = Self::default();
        if let Some(explicit) = explicit_config_path {
            let file_path = if explicit.is_dir() {
                let candidate = explicit.join(CONFIG_FILE_NAME);
                if !candidate.is_file() {
                    return Err(ConfigError::new(format!(
                        "unable to find `{CONFIG_FILE_NAME}` in `{}`",
                        explicit.display()
                    )));
                }
                candidate
            } else {
                if !explicit.is_file() {
                    return Err(ConfigError::new(format!(
                        "configuration file `{}` does not exist",
                        explicit.display()
                    )));
                }
                explicit.to_path_buf()
            };
            config.load_file(&file_path)?;
            return Ok(config);
        }

        let mut cursor = Some(start_dir);
        while let Some(dir) = cursor {
            let candidate = dir.join(CONFIG_FILE_NAME);
            if candidate.is_file() {
                config.load_file(&candidate)?;
                return Ok(config);
            }
            cursor = dir.parent();
        }

        Ok(config)
    }

    /// Load and apply a `nessafmt.toml` file.
    pub fn load_file(&mut self, path: &Path) -> Result<(), ConfigError> {
        let content = fs::read_to_string(path).map_err(|err| {
            ConfigError::new(format!("failed to read `{}`: {err}", path.display()))
        })?;
        self.parse_toml(&content, &path.display().to_string())?;
        self.config_dir = path.parent().map(Path::to_path_buf);
        Ok(())
    }

    /// Parse `nessafmt.toml` content into `self`.
    pub fn parse_toml(&mut self, src: &str, origin: &str) -> Result<(), ConfigError> {
        let entries = parse_toml_top_level_entries(src, origin)?;
        for (key, val) in entries {
            self.apply_toml_entry(&key, &val, origin)?;
        }
        Ok(())
    }

    fn apply_toml_entry(&mut self, key: &str, val: &str, origin: &str) -> Result<(), ConfigError> {
        let val = val.trim();
        match key {
            "max_width" => {
                let n = parse_positive_usize(val).ok_or_else(|| {
                    ConfigError::new(format!(
                        "`{origin}`: `max_width` expects a positive integer, found `{val}`"
                    ))
                })?;
                self.max_width = n;
            }
            "tab_spaces" => {
                let n = parse_positive_usize(val).ok_or_else(|| {
                    ConfigError::new(format!(
                        "`{origin}`: `tab_spaces` expects a positive integer, found `{val}`"
                    ))
                })?;
                self.tab_spaces = n;
            }
            "newline_style" => {
                let quoted_or_sym = strip_matching_quotes(val).is_some() || val.starts_with('.');
                let style = if quoted_or_sym {
                    NewlineStyle::parse_value(val)
                } else {
                    None
                }
                .ok_or_else(|| {
                    ConfigError::new(format!(
                        "`{origin}`: `newline_style` expects `\"lf\"`, `\"crlf\"`, or `\"auto\"`, found `{val}`"
                    ))
                })?;
                self.newline_style = style;
            }
            "use_small_heuristics" => {
                let quoted_or_sym = strip_matching_quotes(val).is_some() || val.starts_with('.');
                let mode = if quoted_or_sym {
                    SmallHeuristics::parse_value(val)
                } else {
                    None
                }
                .ok_or_else(|| {
                    ConfigError::new(format!(
                        "`{origin}`: `use_small_heuristics` expects `\"default\"`, `\"off\"`, or `\"max\"`, found `{val}`"
                    ))
                })?;
                self.use_small_heuristics = mode;
            }
            "reorder_imports" => {
                let b = parse_bool_literal(val).ok_or_else(|| {
                    ConfigError::new(format!(
                        "`{origin}`: `reorder_imports` expects `true` or `false`, found `{val}`"
                    ))
                })?;
                self.reorder_imports = b;
            }
            "normalize_comments" => {
                let b = parse_bool_literal(val).ok_or_else(|| {
                    ConfigError::new(format!(
                        "`{origin}`: `normalize_comments` expects `true` or `false`, found `{val}`"
                    ))
                })?;
                self.normalize_comments = b;
            }
            "ignore" => {
                let list = parse_toml_string_array(val).ok_or_else(|| {
                    ConfigError::new(format!(
                        "`{origin}`: `ignore` expects an array of strings `[\"pattern\", ...]`, found `{val}`"
                    ))
                })?;
                self.ignore = list;
            }
            other => {
                return Err(ConfigError::new(format!(
                    "`{origin}`: unknown configuration option `{other}`"
                )));
            }
        }
        Ok(())
    }

    /// Apply CLI `--config` overrides. Accepts both `key=val,k2=v2` and `.key val, .k2 val2`.
    pub fn apply_cli_overrides(&mut self, spec: &str) -> Result<(), ConfigError> {
        let entries = split_top_level_commas(spec)?;
        for raw_entry in entries {
            let entry = raw_entry.trim();
            if entry.is_empty() {
                continue;
            }
            if let Some(rest) = entry.strip_prefix('.') {
                self.apply_dot_override(rest, "--config")?;
            } else if let Some((k, v)) = split_key_eq_val(entry) {
                self.apply_key_value_str(k.trim(), v.trim(), "--config")?;
            } else {
                return Err(ConfigError::new(format!(
                    "invalid `--config` entry `{entry}`: expected `key=value` or `.key value`"
                )));
            }
        }
        Ok(())
    }

    /// Apply a single `.key value` (or boolean `.key`) override from `--config`.
    pub fn apply_dot_override(&mut self, rest: &str, origin: &str) -> Result<(), ConfigError> {
        let rest = rest.trim();
        let split_idx = rest
            .find(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or(rest.len());
        let key = &rest[..split_idx];
        let mut val = rest[split_idx..].trim();
        if let Some(after_eq) = val.strip_prefix('=') {
            val = after_eq.trim();
        }
        if key.is_empty() {
            return Err(ConfigError::new(format!(
                "`{origin}`: empty configuration key"
            )));
        }
        if val.is_empty() {
            match key {
                "reorder_imports" => {
                    self.reorder_imports = true;
                    return Ok(());
                }
                "normalize_comments" => {
                    self.normalize_comments = true;
                    return Ok(());
                }
                _ => {
                    return Err(ConfigError::new(format!(
                        "`{origin}`: option `.{key}` requires a value"
                    )));
                }
            }
        }
        self.apply_key_value_str(key, val, origin)
    }

    pub fn apply_key_value_str(
        &mut self,
        key: &str,
        val: &str,
        origin: &str,
    ) -> Result<(), ConfigError> {
        let key = key.strip_prefix('.').unwrap_or(key);
        match key {
            "max_width" => {
                let n = parse_positive_usize(val).ok_or_else(|| {
                    ConfigError::new(format!(
                        "`{origin}`: `max_width` expects a positive integer, found `{val}`"
                    ))
                })?;
                self.max_width = n;
            }
            "tab_spaces" => {
                let n = parse_positive_usize(val).ok_or_else(|| {
                    ConfigError::new(format!(
                        "`{origin}`: `tab_spaces` expects a positive integer, found `{val}`"
                    ))
                })?;
                self.tab_spaces = n;
            }
            "newline_style" => {
                let style = NewlineStyle::parse_value(val).ok_or_else(|| {
                    ConfigError::new(format!(
                        "`{origin}`: `newline_style` expects `lf`, `crlf`, or `auto`, found `{val}`"
                    ))
                })?;
                self.newline_style = style;
            }
            "use_small_heuristics" => {
                let mode = SmallHeuristics::parse_value(val).ok_or_else(|| {
                    ConfigError::new(format!(
                        "`{origin}`: `use_small_heuristics` expects `default`, `off`, or `max`, found `{val}`"
                    ))
                })?;
                self.use_small_heuristics = mode;
            }
            "reorder_imports" => {
                let b = parse_bool_literal(val).ok_or_else(|| {
                    ConfigError::new(format!(
                        "`{origin}`: `reorder_imports` expects `true` or `false`, found `{val}`"
                    ))
                })?;
                self.reorder_imports = b;
            }
            "normalize_comments" => {
                let b = parse_bool_literal(val).ok_or_else(|| {
                    ConfigError::new(format!(
                        "`{origin}`: `normalize_comments` expects `true` or `false`, found `{val}`"
                    ))
                })?;
                self.normalize_comments = b;
            }
            "ignore" => {
                let list = parse_cli_string_list(val).ok_or_else(|| {
                    ConfigError::new(format!(
                        "`{origin}`: `ignore` expects `[\"pattern\", ...]`, found `{val}`"
                    ))
                })?;
                self.ignore = list;
            }
            other => {
                return Err(ConfigError::new(format!(
                    "`{origin}`: unknown configuration option `{other}`"
                )));
            }
        }
        Ok(())
    }

    /// Format configuration as valid `nessafmt.toml` text.
    pub fn to_toml_string(&self, minimal: bool) -> String {
        let def = Self::default();
        let mut lines = Vec::new();
        if !minimal || self.max_width != def.max_width {
            lines.push(format!("max_width = {}", self.max_width));
        }
        if !minimal || self.tab_spaces != def.tab_spaces {
            lines.push(format!("tab_spaces = {}", self.tab_spaces));
        }
        if !minimal || self.newline_style != def.newline_style {
            lines.push(format!(
                "newline_style = \"{}\"",
                self.newline_style.as_toml_str()
            ));
        }
        if !minimal || self.use_small_heuristics != def.use_small_heuristics {
            lines.push(format!(
                "use_small_heuristics = \"{}\"",
                self.use_small_heuristics.as_toml_str()
            ));
        }
        if !minimal || self.reorder_imports != def.reorder_imports {
            lines.push(format!("reorder_imports = {}", self.reorder_imports));
        }
        if !minimal || self.normalize_comments != def.normalize_comments {
            lines.push(format!("normalize_comments = {}", self.normalize_comments));
        }
        if !minimal || self.ignore != def.ignore {
            if self.ignore.is_empty() {
                lines.push("ignore = []".to_string());
            } else {
                let quoted: Vec<String> = self
                    .ignore
                    .iter()
                    .map(|s| format!("\"{}\"", escape_toml_string(s)))
                    .collect();
                lines.push(format!("ignore = [{}]", quoted.join(", ")));
            }
        }
        if lines.is_empty() {
            String::new()
        } else {
            let mut out = lines.join("\n");
            out.push('\n');
            out
        }
    }

    /// Check whether `path` is excluded by `ignore` patterns.
    pub fn is_ignored(&self, path: &Path, scan_root: &Path) -> bool {
        if self.ignore.is_empty() {
            return false;
        }
        let mut candidates = Vec::new();
        if let Some(cfg_dir) = &self.config_dir
            && let Ok(rel) = path.strip_prefix(cfg_dir)
        {
            candidates.push(normalize_slash_path(rel));
        }
        if let Ok(rel) = path.strip_prefix(scan_root) {
            let norm = normalize_slash_path(rel);
            if !candidates.contains(&norm) {
                candidates.push(norm);
            }
        }
        let raw_norm = normalize_slash_path(path);
        if !candidates.contains(&raw_norm) {
            candidates.push(raw_norm);
        }

        for pattern in &self.ignore {
            let pat = pattern.trim();
            if pat.is_empty() {
                continue;
            }
            let pat = pat.strip_prefix("./").unwrap_or(pat);
            for candidate in &candidates {
                if candidate.is_empty() {
                    continue;
                }
                if matches_ignore_pattern(pat, candidate) {
                    return true;
                }
            }
        }
        false
    }
}

fn normalize_slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn matches_ignore_pattern(pattern: &str, candidate: &str) -> bool {
    if let Some(dir_pat) = pattern.strip_suffix('/') {
        return candidate == dir_pat
            || candidate.starts_with(&format!("{dir_pat}/"))
            || glob_match(dir_pat, candidate)
            || glob_match(&format!("{dir_pat}/**"), candidate);
    }
    if !pattern.contains('/') {
        for seg in candidate.split('/') {
            if seg == pattern || glob_match(pattern, seg) {
                return true;
            }
        }
    }
    if !pattern.contains('*')
        && !pattern.contains('?')
        && (candidate == pattern || candidate.starts_with(&format!("{pattern}/")))
    {
        return true;
    }
    glob_match(pattern, candidate)
}

/// Glob matcher supporting `*`, `**`, and `?` over `/`-separated paths.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    glob_match_inner(pattern.as_bytes(), text.as_bytes())
}

fn glob_match_inner(pat: &[u8], text: &[u8]) -> bool {
    if pat.is_empty() {
        return text.is_empty();
    }
    if pat.starts_with(b"**/") {
        if glob_match_inner(&pat[3..], text) {
            return true;
        }
        for i in 0..text.len() {
            if text[i] == b'/' && glob_match_inner(&pat[3..], &text[i + 1..]) {
                return true;
            }
        }
        return false;
    }
    if pat == b"**" {
        return true;
    }
    if pat.starts_with(b"**") {
        return glob_match_inner(&pat[2..], text)
            || (!text.is_empty() && glob_match_inner(pat, &text[1..]));
    }
    if pat[0] == b'*' {
        if glob_match_inner(&pat[1..], text) {
            return true;
        }
        if !text.is_empty() && text[0] != b'/' {
            return glob_match_inner(pat, &text[1..]);
        }
        return false;
    }
    if pat[0] == b'?' {
        if !text.is_empty() && text[0] != b'/' {
            return glob_match_inner(&pat[1..], &text[1..]);
        }
        return false;
    }
    if !text.is_empty() && pat[0] == text[0] {
        return glob_match_inner(&pat[1..], &text[1..]);
    }
    false
}

/// Parse top-level `key = value` entries from a `nessafmt.toml` document.
fn parse_toml_top_level_entries(
    src: &str,
    origin: &str,
) -> Result<Vec<(String, String)>, ConfigError> {
    let cleaned = strip_toml_comments_and_validate(src, origin)?;
    let mut entries = Vec::new();
    let bytes = cleaned.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;

    while i < len {
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len {
            break;
        }
        if bytes[i] == b'[' {
            return Err(ConfigError::new(format!(
                "`{origin}`: unexpected table header `[...]` (all `nessafmt.toml` keys are top-level)"
            )));
        }

        let key_start = i;
        while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'-')
        {
            i += 1;
        }
        let key = cleaned[key_start..i].trim();
        if key.is_empty() {
            let line_rest = cleaned[key_start..].lines().next().unwrap_or("");
            return Err(ConfigError::new(format!(
                "`{origin}`: syntax error near `{line_rest}`"
            )));
        }

        while i < len && matches!(bytes[i], b' ' | b'\t') {
            i += 1;
        }
        if i >= len || bytes[i] != b'=' {
            return Err(ConfigError::new(format!(
                "`{origin}`: expected `=` after key `{key}`"
            )));
        }
        i += 1; // skip `=`
        while i < len && matches!(bytes[i], b' ' | b'\t') {
            i += 1;
        }

        let val_start = i;
        let mut bracket_depth = 0usize;
        let mut in_double = false;
        let mut in_single = false;
        let mut escaped = false;

        while i < len {
            let ch = cleaned[i..].chars().next().unwrap();
            let ch_len = ch.len_utf8();
            if in_double {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_double = false;
                }
                i += ch_len;
                continue;
            }
            if in_single {
                if ch == '\'' {
                    in_single = false;
                }
                i += ch_len;
                continue;
            }
            match ch {
                '"' => in_double = true,
                '\'' => in_single = true,
                '[' => bracket_depth += 1,
                ']' => {
                    if bracket_depth == 0 {
                        return Err(ConfigError::new(format!(
                            "`{origin}`: unmatched `]` in value for `{key}`"
                        )));
                    }
                    bracket_depth -= 1;
                }
                '\n' if bracket_depth == 0 => break,
                _ => {}
            }
            i += ch_len;
        }

        if bracket_depth != 0 {
            return Err(ConfigError::new(format!(
                "`{origin}`: unclosed `[` in value for `{key}`"
            )));
        }

        let val = cleaned[val_start..i].trim();
        if val.is_empty() {
            return Err(ConfigError::new(format!(
                "`{origin}`: missing value for `{key}`"
            )));
        }
        entries.push((key.to_string(), val.to_string()));
    }

    Ok(entries)
}

fn strip_toml_comments_and_validate(src: &str, origin: &str) -> Result<String, ConfigError> {
    let mut out = String::with_capacity(src.len());
    let mut in_double = false;
    let mut in_single = false;
    let mut escaped = false;
    let mut chars = src.chars().peekable();

    while let Some(ch) = chars.next() {
        if in_double {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_double = false;
            } else if ch == '\n' {
                return Err(ConfigError::new(format!(
                    "`{origin}`: unterminated string literal"
                )));
            }
            continue;
        }
        if in_single {
            out.push(ch);
            if ch == '\'' {
                in_single = false;
            } else if ch == '\n' {
                return Err(ConfigError::new(format!(
                    "`{origin}`: unterminated literal string"
                )));
            }
            continue;
        }
        match ch {
            '"' => {
                in_double = true;
                out.push(ch);
            }
            '\'' => {
                in_single = true;
                out.push(ch);
            }
            '#' => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            _ => out.push(ch),
        }
    }

    if in_double || in_single {
        return Err(ConfigError::new(format!(
            "`{origin}`: unterminated string literal"
        )));
    }
    Ok(out)
}

fn strip_matching_quotes(s: &str) -> Option<&str> {
    if s.len() >= 2 {
        if s.starts_with('"') && s.ends_with('"') {
            return Some(&s[1..s.len() - 1]);
        }
        if s.starts_with('\'') && s.ends_with('\'') {
            return Some(&s[1..s.len() - 1]);
        }
    }
    None
}

fn parse_positive_usize(raw: &str) -> Option<usize> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.starts_with('_') || trimmed.ends_with('_') {
        return None;
    }
    let cleaned: String = trimmed.chars().filter(|&c| c != '_').collect();
    let val: usize = cleaned.parse().ok()?;
    (val >= 1).then_some(val)
}

fn parse_bool_literal(raw: &str) -> Option<bool> {
    match raw.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn parse_toml_string_array(raw: &str) -> Option<Vec<String>> {
    let inner = raw.trim().strip_prefix('[')?.strip_suffix(']')?.trim();
    if inner.is_empty() {
        return Some(Vec::new());
    }
    let items = split_top_level_commas(inner).ok()?;
    let mut out = Vec::new();
    for item in items {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if item.starts_with('"') && item.ends_with('"') {
            out.push(unescape_toml_basic_string(item)?);
        } else if item.starts_with('\'') && item.ends_with('\'') && item.len() >= 2 {
            out.push(item[1..item.len() - 1].to_string());
        } else {
            return None;
        }
    }
    Some(out)
}

fn parse_cli_string_list(raw: &str) -> Option<Vec<String>> {
    let inner = raw.trim().strip_prefix('[')?.strip_suffix(']')?.trim();
    if inner.is_empty() {
        return Some(Vec::new());
    }
    let items = split_top_level_commas(inner).ok()?;
    let mut out = Vec::new();
    for item in items {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if item.starts_with('"') && item.ends_with('"') {
            out.push(unescape_toml_basic_string(item)?);
        } else if item.starts_with('\'') && item.ends_with('\'') && item.len() >= 2 {
            out.push(item[1..item.len() - 1].to_string());
        } else {
            out.push(item.to_string());
        }
    }
    Some(out)
}

pub fn split_top_level_commas(input: &str) -> Result<Vec<&str>, ConfigError> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut in_double = false;
    let mut in_single = false;
    let mut escaped = false;
    let mut bracket_depth = 0usize;

    for (idx, ch) in input.char_indices() {
        if in_double {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_double = false;
            }
            continue;
        }
        if in_single {
            if ch == '\'' {
                in_single = false;
            }
            continue;
        }
        match ch {
            '"' => in_double = true,
            '\'' => in_single = true,
            '[' | '(' | '{' => bracket_depth += 1,
            ']' | ')' | '}' => {
                if bracket_depth == 0 {
                    return Err(ConfigError::new(format!(
                        "unmatched closing delimiter `{ch}` in `{input}`"
                    )));
                }
                bracket_depth -= 1;
            }
            ',' if bracket_depth == 0 => {
                parts.push(&input[start..idx]);
                start = idx + 1;
            }
            _ => {}
        }
    }
    if in_double || in_single {
        return Err(ConfigError::new(format!(
            "unterminated string literal in `{input}`"
        )));
    }
    if bracket_depth != 0 {
        return Err(ConfigError::new(format!(
            "unclosed bracket delimiter in `{input}`"
        )));
    }
    parts.push(&input[start..]);
    Ok(parts)
}

fn split_key_eq_val(entry: &str) -> Option<(&str, &str)> {
    let eq_pos = entry.find('=')?;
    let key = &entry[..eq_pos];
    let val = &entry[eq_pos + 1..];
    if key.trim().is_empty() {
        return None;
    }
    Some((key, val))
}

fn unescape_toml_basic_string(quoted: &str) -> Option<String> {
    let inner = quoted.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next()? {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            '0' => out.push('\0'),
            'u' => {
                let mut hex = String::with_capacity(4);
                for _ in 0..4 {
                    hex.push(chars.next()?);
                }
                let cp = u32::from_str_radix(&hex, 16).ok()?;
                out.push(char::from_u32(cp)?);
            }
            'U' => {
                let mut hex = String::with_capacity(8);
                for _ in 0..8 {
                    hex.push(chars.next()?);
                }
                let cp = u32::from_str_radix(&hex, 16).ok()?;
                out.push(char::from_u32(cp)?);
            }
            _ => return None,
        }
    }
    Some(out)
}

fn escape_toml_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}
