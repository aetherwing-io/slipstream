//! Compact FCP-style response formatting for Slipstream MCP.
//!
//! Converts daemon JSON responses into terse prefixed-line format:
//!   + file opened, ~ edit applied, > flushed, ! error, @ read, - closed
//!
//! Every response ends with a status bar: [Nf Ne flush:state closed|sess:NAME]

use serde_json::Value;
use std::fmt::Write;

/// Check if a daemon response is an FCP pass-through (verbatim from FCP handler).
pub fn is_fcp_passthrough(value: &Value) -> bool {
    value.get("fcp_passthrough").is_some()
}

/// Extract the FCP pass-through text from a response, returning it verbatim.
/// Returns the `text` field if present, otherwise the JSON as-is.
pub fn format_fcp_passthrough(value: &Value) -> String {
    if let Some(text) = value.get("text").and_then(|t| t.as_str()) {
        text.to_string()
    } else {
        // Return the whole result minus the fcp_passthrough marker
        let mut v = value.clone();
        if let Some(obj) = v.as_object_mut() {
            obj.remove("fcp_passthrough");
        }
        serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())
    }
}

/// Accumulated state for building the status bar.
#[derive(Default)]
pub struct StatusBar {
    pub files: usize,
    pub edits: usize,
    pub flush: FlushState,
    pub session: SessionState,
    pub dirty: usize,
    pub errors: usize,
}

#[derive(Default)]
pub enum FlushState {
    #[default]
    Skip,
    Ok,
    Conflict,
}

pub enum SessionState {
    Closed,
    Named(String),
}

impl Default for SessionState {
    fn default() -> Self {
        SessionState::Closed
    }
}

impl std::fmt::Display for StatusBar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}f {}e", self.files, self.edits)?;
        match self.flush {
            FlushState::Ok => write!(f, " flush:ok")?,
            FlushState::Conflict => write!(f, " flush:conflict")?,
            FlushState::Skip => write!(f, " flush:skip")?,
        }
        match &self.session {
            SessionState::Closed => write!(f, " closed")?,
            SessionState::Named(n) => write!(f, " sess:{n}")?,
        }
        if self.dirty > 0 {
            write!(f, " {}d", self.dirty)?;
        }
        if self.errors > 0 {
            write!(f, " !{}err", self.errors)?;
        }
        write!(f, "]")
    }
}

/// Format a one-shot response (open → batch? → read? → close).
/// `ops` is the original ops array sent to the daemon (for path/method context).
pub fn format_one_shot(output: &Value, ops: Option<&Value>, read_all: bool) -> String {
    // If the open result is an FCP pass-through, return it verbatim
    if let Some(open) = output.get("open") {
        if is_fcp_passthrough(open) {
            return format_fcp_passthrough(open);
        }
    }

    let mut lines = Vec::new();
    let mut bar = StatusBar::default();

    // Count files from open
    if let Some(open) = output.get("open") {
        if let Some(files) = open.get("files").and_then(|f| f.as_object()) {
            bar.files = files.len();
        }
    }

    // Read results (if read_all) — batch response is a flat array
    if read_all {
        if let Some(read) = output.get("read") {
            format_batch_array(read, None, &mut lines, &mut bar);
        }
    }

    // Batch edit results — correlate with ops for path/method info
    if let Some(batch) = output.get("batch") {
        format_batch_array(batch, ops, &mut lines, &mut bar);
    }

    // Flush info — either nested in close or top-level (CLI exec path)
    if let Some(flush) = output.get("flush") {
        format_flush_result(flush, &mut lines, &mut bar);
    }
    if let Some(close) = output.get("close") {
        format_close_result(close, &mut lines, &mut bar);
    }

    bar.session = SessionState::Closed;
    lines.push(bar.to_string());
    lines.join("\n")
}

/// Format session ops response (batch + optional flush, session stays open).
pub fn format_session_ops(output: &Value, ops: &Value, session_name: &str) -> String {
    let mut lines = Vec::new();
    let mut bar = StatusBar::default();
    bar.session = SessionState::Named(session_name.to_string());

    if let Some(batch) = output.get("batch") {
        format_batch_array(batch, Some(ops), &mut lines, &mut bar);
    }

    if let Some(flush) = output.get("flush") {
        format_flush_result(flush, &mut lines, &mut bar);
    }

    lines.push(bar.to_string());
    lines.join("\n")
}

/// Format session open response.
pub fn format_open(value: &Value, session_name: &str) -> String {
    let mut lines = Vec::new();
    let mut bar = StatusBar::default();
    bar.session = SessionState::Named(session_name.to_string());

    if let Some(files) = value.get("files").and_then(|f| f.as_object()) {
        bar.files = files.len();
        for (path, info) in files {
            let line_count = info.get("lines").and_then(|l| l.as_u64()).unwrap_or(0);
            let version = info.get("version").and_then(|v| v.as_u64()).unwrap_or(1);
            let short = short_path(path);
            lines.push(format!("+ {short} ({line_count}L v{version})"));
        }
    }

    lines.push(bar.to_string());
    lines.join("\n")
}

/// Format session close response.
pub fn format_close(value: &Value) -> String {
    let mut lines = Vec::new();
    let mut bar = StatusBar::default();
    bar.session = SessionState::Closed;

    format_close_result(value, &mut lines, &mut bar);

    lines.push("- closed".to_string());
    lines.push(bar.to_string());
    lines.join("\n")
}

/// Format session close --no-flush response.
pub fn format_close_no_flush(_value: &Value) -> String {
    let mut lines = Vec::new();
    let mut bar = StatusBar::default();
    bar.session = SessionState::Closed;
    bar.flush = FlushState::Skip;

    lines.push("- closed".to_string());
    lines.push(bar.to_string());
    lines.join("\n")
}

/// Format flush response.
pub fn format_flush(value: &Value, session_name: &str) -> String {
    let mut lines = Vec::new();
    let mut bar = StatusBar::default();
    bar.session = SessionState::Named(session_name.to_string());

    format_flush_result(value, &mut lines, &mut bar);

    lines.push(bar.to_string());
    lines.join("\n")
}

/// Format read response — header + raw content.
pub fn format_read(value: &Value, path: &str, start: Option<usize>, end: Option<usize>) -> String {
    let mut out = String::new();

    let read_lines = value.get("lines").and_then(|l| l.as_array());
    let cursor = value.get("cursor").and_then(|c| c.as_u64()).unwrap_or(0);
    let line_count = read_lines.map(|l| l.len()).unwrap_or(0);
    let short = short_path(path);

    if let (Some(s), Some(e)) = (start, end) {
        let _ = write!(out, "@ {short}:{s}-{e} ({line_count}L cursor:{cursor})");
    } else if let Some(s) = start {
        let _ = write!(out, "@ {short}:{s}- ({line_count}L cursor:{cursor})");
    } else {
        let _ = write!(out, "@ {short} ({line_count}L cursor:{cursor})");
    }

    if let Some(arr) = read_lines {
        for line in arr {
            out.push('\n');
            if let Some(s) = line.as_str() {
                out.push_str(s);
            }
        }
    }

    out
}

/// Format batch results from a flat JSON array, correlating with ops for context.
/// The daemon returns batch results as `[{match_line, match_count, ...}, ...]`
/// without method/path — those come from the original ops array.
fn format_batch_array(batch: &Value, ops: Option<&Value>, lines: &mut Vec<String>, bar: &mut StatusBar) {
    let results = match batch.as_array() {
        Some(r) => r,
        None => return,
    };
    let ops_arr = ops.and_then(|o| o.as_array());

    for (i, result) in results.iter().enumerate() {
        let op = ops_arr.and_then(|arr| arr.get(i));
        let path = op.and_then(|o| o.get("path")).and_then(|p| p.as_str()).unwrap_or("?");
        let method = op.and_then(|o| o.get("method")).and_then(|m| m.as_str()).unwrap_or("");
        let short = short_path(path);

        // Check for error result
        if let Some(err) = result.get("error").and_then(|e| e.as_str()) {
            lines.push(format!("! {short} {method}: {err}"));
            bar.errors += 1;
            continue;
        }

        // Detect result type from fields present
        if result.get("match_line").is_some() {
            // str_replace result
            let match_line = result.get("match_line").and_then(|l| l.as_u64()).unwrap_or(0);
            let match_count = result.get("match_count").and_then(|c| c.as_u64()).unwrap_or(1);
            let replace_all = op.and_then(|o| o.get("replace_all")).and_then(|r| r.as_bool()).unwrap_or(false);
            let mut desc = format!("~ {short}:{match_line} str_replace ({match_count} match");
            if match_count != 1 {
                desc.push_str("es");
            }
            if replace_all && match_count > 1 {
                desc.push_str(", replace_all");
            }
            desc.push(')');
            lines.push(desc);
            // Inline diff: show old/new lines
            if let (Some(old), Some(new)) = (
                op.and_then(|o| o.get("old_str")).and_then(|s| s.as_str()),
                op.and_then(|o| o.get("new_str")).and_then(|s| s.as_str()),
            ) {
                format_inline_diff(old, new, lines);
            }
            bar.edits += 1;
        } else if result.get("lines").is_some() {
            // read result
            let read_lines = result.get("lines").and_then(|l| l.as_array());
            let cursor = result.get("cursor").and_then(|c| c.as_u64()).unwrap_or(0);
            let line_count = read_lines.map(|l| l.len()).unwrap_or(0);
            lines.push(format!("@ {short} ({line_count}L cursor:{cursor})"));
            if let Some(content) = read_lines {
                for line in content {
                    if let Some(s) = line.as_str() {
                        lines.push(s.to_string());
                    }
                }
            }
        } else if result.get("edits_pending").is_some() && method.contains("write") {
            // write result
            let start = op.and_then(|o| o.get("start")).and_then(|s| s.as_u64()).unwrap_or(0);
            let end = op.and_then(|o| o.get("end")).and_then(|e| e.as_u64()).unwrap_or(0);
            let content_len = op.and_then(|o| o.get("content")).and_then(|c| c.as_array()).map(|a| a.len()).unwrap_or(0);
            if start == end {
                lines.push(format!("~ {short}:{start} write {content_len}L inserted"));
            } else {
                lines.push(format!("~ {short}:{start}-{end} write {content_len}L replaced"));
            }
            bar.edits += 1;
        } else if result.get("edits_pending").is_some() {
            // Generic write/edit result without method context
            lines.push(format!("~ {short} edit applied"));
            bar.edits += 1;
        }
    }
}

/// Extract flush info from a close result and add `>` lines.
fn format_close_result(close: &Value, lines: &mut Vec<String>, bar: &mut StatusBar) {
    if let Some(flush) = close.get("flush") {
        format_flush_result(flush, lines, bar);
    }
}

/// Format flush result — `>` lines for files written, `!` for warnings.
fn format_flush_result(flush: &Value, lines: &mut Vec<String>, bar: &mut StatusBar) {
    if let Some(files) = flush.get("files_written").and_then(|f| f.as_array()) {
        if !files.is_empty() {
            bar.flush = FlushState::Ok;
            let parts: Vec<String> = files.iter().map(|f| {
                let path = f.get("path").and_then(|p| p.as_str()).unwrap_or("?");
                let edits = f.get("edits_applied").and_then(|e| e.as_u64()).unwrap_or(0);
                let short = short_path(path);
                format!("{short} ({edits} edit{})", if edits != 1 { "s" } else { "" })
            }).collect();
            lines.push(format!("> {}", parts.join(" · ")));
        }
    }

    if let Some(warnings) = flush.get("warnings").and_then(|w| w.as_array()) {
        for w in warnings {
            let path = w.get("path").and_then(|p| p.as_str()).unwrap_or("?");
            let other = w.get("other_session").and_then(|o| o.as_str()).unwrap_or("?");
            let count = w.get("pending_edit_count").and_then(|c| c.as_u64()).unwrap_or(0);
            let short = short_path(path);
            lines.push(format!("! {short} flush warning: {count} pending edits in session \"{other}\""));
        }
    }
}

/// Format an RPC error into compact `!` lines.
pub fn format_rpc_error(code: i64, message: &str, data: Option<&Value>) -> String {
    let mut lines = Vec::new();

    // Check for conflict errors with structured data
    if let Some(conflicts) = data.and_then(|d| d.as_array()) {
        for c in conflicts {
            let path = c.get("path").and_then(|p| p.as_str()).unwrap_or("?");
            let by = c.get("by_session").and_then(|s| s.as_str()).unwrap_or("?");
            let ranges = c.get("conflicting_edits")
                .and_then(|r| r.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|r| {
                            let start = r.get(0).and_then(|s| s.as_u64())?;
                            let end = r.get(1).and_then(|e| e.as_u64())?;
                            Some(format!("{start}-{end}"))
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            let short = short_path(path);
            lines.push(format!("! {short} flush: conflict lines [{ranges}] by session \"{by}\""));
        }
    } else {
        lines.push(format!("! error {code}: {message}"));
    }

    lines.join("\n")
}

/// Format an inline diff showing old/new lines.
/// For short edits (≤3 lines each), show all lines.
/// For longer edits, show first 2 lines + "... (+N more)".
fn format_inline_diff(old: &str, new: &str, lines: &mut Vec<String>) {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let max_show = 3;

    for (i, line) in old_lines.iter().enumerate() {
        if i >= max_show {
            lines.push(format!("  - ... (+{} more)", old_lines.len() - max_show));
            break;
        }
        lines.push(format!("  - {line}"));
    }
    for (i, line) in new_lines.iter().enumerate() {
        if i >= max_show {
            lines.push(format!("  + ... (+{} more)", new_lines.len() - max_show));
            break;
        }
        lines.push(format!("  + {line}"));
    }
}

/// Shorten a path for display — strip common prefixes, keep filename + parent.
fn short_path(path: &str) -> &str {
    // If it's already short, keep it
    if !path.contains('/') {
        return path;
    }
    // Return last component(s) — keep parent/file for disambiguation
    let parts: Vec<&str> = path.rsplitn(3, '/').collect();
    if parts.len() >= 2 {
        // Find where the last 2 components start
        let file = parts[0];
        let parent = parts[1];
        let suffix_len = parent.len() + 1 + file.len();
        &path[path.len() - suffix_len..]
    } else {
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_short_path() {
        assert_eq!(short_path("main.rs"), "main.rs");
        assert_eq!(short_path("src/main.rs"), "src/main.rs");
        assert_eq!(short_path("/home/user/projects/src/main.rs"), "src/main.rs");
    }

    #[test]
    fn test_status_bar_basic() {
        let bar = StatusBar {
            files: 1,
            edits: 1,
            flush: FlushState::Ok,
            session: SessionState::Closed,
            ..Default::default()
        };
        assert_eq!(bar.to_string(), "[1f 1e flush:ok closed]");
    }

    #[test]
    fn test_status_bar_with_errors() {
        let bar = StatusBar {
            files: 1,
            edits: 2,
            flush: FlushState::Skip,
            session: SessionState::Closed,
            errors: 1,
            ..Default::default()
        };
        assert_eq!(bar.to_string(), "[1f 2e flush:skip closed !1err]");
    }

    #[test]
    fn test_status_bar_session() {
        let bar = StatusBar {
            files: 2,
            edits: 0,
            flush: FlushState::Skip,
            session: SessionState::Named("default".to_string()),
            ..Default::default()
        };
        assert_eq!(bar.to_string(), "[2f 0e flush:skip sess:default]");
    }

    #[test]
    fn test_format_open() {
        let val = json!({
            "session_id": "test",
            "files": {
                "main.rs": { "lines": 150, "version": 1 },
                "lib.rs": { "lines": 200, "version": 1 },
            }
        });
        let out = format_open(&val, "default");
        assert!(out.contains("+ main.rs (150L v1)") || out.contains("+ lib.rs (200L v1)"));
        assert!(out.contains("[2f 0e flush:skip sess:default]"));
    }

    #[test]
    fn test_format_one_shot_quick_edit() {
        let ops = json!([{
            "method": "file.str_replace",
            "path": "f.rs",
            "old_str": "foo",
            "new_str": "bar"
        }]);
        let output = json!({
            "open": {
                "session_id": "test",
                "files": { "f.rs": { "lines": 100, "version": 1 } }
            },
            "batch": [{
                "match_line": 42,
                "match_count": 1,
                "edits_pending": 1
            }],
            "close": {
                "status": "closed",
                "flush": {
                    "files_written": [{ "path": "f.rs", "edits_applied": 1 }],
                    "warnings": []
                }
            }
        });
        let out = format_one_shot(&output, Some(&ops), false);
        assert!(out.contains("~ f.rs:42 str_replace (1 match)"), "got: {out}");
        assert!(out.contains("> f.rs (1 edit)"), "got: {out}");
        assert!(out.contains("[1f 1e flush:ok closed]"), "got: {out}");
    }

    #[test]
    fn test_format_read() {
        let val = json!({
            "lines": ["fn main() {", "    println!(\"hello\");", "}"],
            "cursor": 13,
            "other_sessions": []
        });
        let out = format_read(&val, "src/main.rs", Some(10), Some(13));
        assert!(out.starts_with("@ src/main.rs:10-13 (3L cursor:13)"), "got: {out}");
        assert!(out.contains("fn main() {"));
    }

    #[test]
    fn test_format_rpc_error_conflict() {
        let data = json!([{
            "path": "main.rs",
            "your_edits": [[10, 15]],
            "conflicting_edits": [[100, 105]],
            "by_session": "agent-2",
            "hint": "..."
        }]);
        let out = format_rpc_error(-32001, "conflicting edits", Some(&data));
        assert!(out.contains("! main.rs flush: conflict lines [100-105] by session \"agent-2\""), "got: {out}");
    }

    #[test]
    fn test_fcp_passthrough_detection() {
        let normal = json!({"session_id": "abc", "files": {}});
        assert!(!is_fcp_passthrough(&normal));

        let fcp = json!({"text": "Sheet loaded", "fcp_passthrough": "sheets"});
        assert!(is_fcp_passthrough(&fcp));
    }

    #[test]
    fn test_fcp_passthrough_format_text() {
        let fcp = json!({"text": "A1: Revenue\nB1: 100", "fcp_passthrough": "sheets"});
        assert_eq!(format_fcp_passthrough(&fcp), "A1: Revenue\nB1: 100");
    }

    #[test]
    fn test_fcp_passthrough_format_no_text() {
        let fcp = json!({"success": true, "fcp_passthrough": "midi"});
        let out = format_fcp_passthrough(&fcp);
        assert!(out.contains("\"success\": true"), "got: {out}");
        assert!(!out.contains("fcp_passthrough"), "should strip marker, got: {out}");
    }

    #[test]
    fn test_one_shot_fcp_passthrough() {
        let output = json!({
            "open": {
                "text": "Piano loaded",
                "fcp_passthrough": "midi"
            }
        });
        let out = format_one_shot(&output, None, false);
        assert_eq!(out, "Piano loaded");
    }
}
