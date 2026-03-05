//! DSL parsers for session actions and queries.
//!
//! Parses terse string commands like "open src/main.rs as:worker-1" into
//! structured enums that the server can dispatch to JSON-RPC calls.

use std::collections::HashMap;

/// Parsed session lifecycle action (includes query verbs since ss_session merges both).
#[derive(Debug, PartialEq)]
pub enum SessionAction {
    Open {
        files: Vec<String>,
        name: Option<String>,
    },
    Flush {
        name: Option<String>,
        force: bool,
    },
    Close {
        name: Option<String>,
        /// Flush before closing (default: true). Use `--no-flush` to skip.
        flush: bool,
        /// Force flush past conflicts. Use `--force`.
        force: bool,
    },
    Register {
        path: String,
        handler: String,
    },
    Unregister {
        tracking_id: String,
    },
    // Query verbs (merged from slipstream_query)
    Read {
        path: String,
        session: Option<String>,
        start: Option<usize>,
        end: Option<usize>,
        count: Option<usize>,
    },
    Status,
    List,
    Check {
        action: String,
    },
}

/// Parsed read-only query.
#[derive(Debug, PartialEq)]
pub enum Query {
    Read {
        path: String,
        session: Option<String>,
        start: Option<usize>,
        end: Option<usize>,
        count: Option<usize>,
    },
    Status,
    List,
    Check {
        action: String,
    },
}

/// Parse `key:value` pairs from tokens (skipping flags like `--force`).
fn parse_kwargs(tokens: &[&str]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for tok in tokens {
        if let Some((k, v)) = tok.split_once(':') {
            if !k.is_empty() && !v.is_empty() {
                map.insert(k.to_string(), v.to_string());
            }
        }
    }
    map
}

/// Parse a session action string.
///
/// Examples:
/// - `"open src/main.rs src/lib.rs"`
/// - `"open data.csv as:worker-1"`
/// - `"flush"`, `"flush --force"`, `"flush session:worker-1"`
/// - `"close"`, `"close session:worker-1"`
/// - `"register /path/file.xlsx sheets"`
/// - `"unregister ext-001"`
pub fn parse_session_action(input: &str) -> Result<SessionAction, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("empty action string".to_string());
    }

    let tokens: Vec<&str> = input.split_whitespace().collect();
    let verb = tokens[0].to_lowercase();
    let rest = &tokens[1..];

    match verb.as_str() {
        "open" => {
            if rest.is_empty() {
                return Err("open requires at least one file path".to_string());
            }
            let kwargs = parse_kwargs(rest);
            let name = kwargs.get("as").cloned();
            let files: Vec<String> = rest
                .iter()
                .filter(|t| !t.contains(':'))
                .map(|t| t.to_string())
                .collect();
            if files.is_empty() {
                return Err("open requires at least one file path".to_string());
            }
            Ok(SessionAction::Open { files, name })
        }
        "flush" => {
            let kwargs = parse_kwargs(rest);
            let name = kwargs.get("session").cloned();
            let force = rest.iter().any(|t| *t == "--force");
            Ok(SessionAction::Flush { name, force })
        }
        "close" => {
            let kwargs = parse_kwargs(rest);
            let name = kwargs.get("session").cloned();
            let flush = !rest.iter().any(|t| *t == "--no-flush");
            let force = rest.iter().any(|t| *t == "--force");
            Ok(SessionAction::Close { name, flush, force })
        }
        "register" => {
            // register <path> <handler>
            let args: Vec<&str> = rest.iter().copied().collect();
            if args.len() < 2 {
                return Err("register requires <path> <handler>".to_string());
            }
            Ok(SessionAction::Register {
                path: args[0].to_string(),
                handler: args[1].to_string(),
            })
        }
        "unregister" => {
            if rest.is_empty() {
                return Err("unregister requires <tracking_id>".to_string());
            }
            Ok(SessionAction::Unregister {
                tracking_id: rest[0].to_string(),
            })
        }
        // Query verbs (merged from parse_query)
        "read" => {
            if rest.is_empty() {
                return Err("read requires a file path".to_string());
            }
            let path = rest[0].to_string();
            let kwargs = parse_kwargs(&rest[1..]);
            let session = kwargs.get("session").cloned();
            let start = kwargs
                .get("start")
                .map(|v| v.parse::<usize>())
                .transpose()
                .map_err(|_| "invalid start value".to_string())?;
            let end = kwargs
                .get("end")
                .map(|v| v.parse::<usize>())
                .transpose()
                .map_err(|_| "invalid end value".to_string())?;
            let count = kwargs
                .get("count")
                .map(|v| v.parse::<usize>())
                .transpose()
                .map_err(|_| "invalid count value".to_string())?;
            Ok(SessionAction::Read {
                path,
                session,
                start,
                end,
                count,
            })
        }
        "status" => Ok(SessionAction::Status),
        "list" => Ok(SessionAction::List),
        "check" => {
            if rest.is_empty() {
                return Err("check requires an action (e.g. 'check build')".to_string());
            }
            Ok(SessionAction::Check {
                action: rest[0].to_string(),
            })
        }
        other => Err(format!(
            "unknown action '{other}'. Expected: open, flush, close, read, status, list, check, register, unregister"
        )),
    }
}

/// Parse a query string.
///
/// Examples:
/// - `"read src/main.rs"`
/// - `"read src/main.rs start:10 end:20"`
/// - `"read src/main.rs count:50"`
/// - `"read src/main.rs session:worker-1"`
/// - `"status"`
/// - `"list"`
/// - `"check build"`
pub fn parse_query(input: &str) -> Result<Query, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("empty query string".to_string());
    }

    let tokens: Vec<&str> = input.split_whitespace().collect();
    let verb = tokens[0].to_lowercase();
    let rest = &tokens[1..];

    match verb.as_str() {
        "read" => {
            if rest.is_empty() {
                return Err("read requires a file path".to_string());
            }
            // First non-kwarg token is the path
            let path = rest[0].to_string();
            let kwargs = parse_kwargs(&rest[1..]);
            let session = kwargs.get("session").cloned();
            let start = kwargs
                .get("start")
                .map(|v| v.parse::<usize>())
                .transpose()
                .map_err(|_| "invalid start value".to_string())?;
            let end = kwargs
                .get("end")
                .map(|v| v.parse::<usize>())
                .transpose()
                .map_err(|_| "invalid end value".to_string())?;
            let count = kwargs
                .get("count")
                .map(|v| v.parse::<usize>())
                .transpose()
                .map_err(|_| "invalid count value".to_string())?;
            Ok(Query::Read {
                path,
                session,
                start,
                end,
                count,
            })
        }
        "status" => Ok(Query::Status),
        "list" => Ok(Query::List),
        "check" => {
            if rest.is_empty() {
                return Err("check requires an action (e.g. 'check build')".to_string());
            }
            Ok(Query::Check {
                action: rest[0].to_string(),
            })
        }
        other => Err(format!(
            "unknown query '{other}'. Expected: read, status, list, check"
        )),
    }
}

/// Parsed batch operation (from DSL string).
#[derive(Debug, PartialEq)]
pub enum OpDsl {
    StrReplace {
        path: String,
        old_str: String,
        new_str: String,
        replace_all: bool,
    },
    Read {
        path: String,
        start: Option<usize>,
        end: Option<usize>,
        count: Option<usize>,
    },
    Write {
        path: String,
        start: usize,
        end: usize,
        content: String,
    },
    CursorMove {
        path: String,
        to: usize,
    },
}

impl OpDsl {
    /// Convert to JSON value suitable for the daemon batch protocol.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            OpDsl::StrReplace { path, old_str, new_str, replace_all } => {
                let mut v = serde_json::json!({
                    "method": "file.str_replace",
                    "path": path,
                    "old_str": old_str,
                    "new_str": new_str,
                });
                if *replace_all {
                    v["replace_all"] = serde_json::json!(true);
                }
                v
            }
            OpDsl::Read { path, start, end, count } => {
                let mut v = serde_json::json!({
                    "method": "file.read",
                    "path": path,
                });
                if let Some(s) = start { v["start"] = serde_json::json!(s); }
                if let Some(e) = end { v["end"] = serde_json::json!(e); }
                if let Some(c) = count { v["count"] = serde_json::json!(c); }
                v
            }
            OpDsl::Write { path, start, end, content } => {
                let lines: Vec<&str> = content.split('\n').collect();
                serde_json::json!({
                    "method": "file.write",
                    "path": path,
                    "start": start,
                    "end": end,
                    "content": lines,
                })
            }
            OpDsl::CursorMove { path, to } => {
                serde_json::json!({
                    "method": "cursor.move",
                    "path": path,
                    "to": to,
                })
            }
        }
    }
}

/// Tokenize an op string, respecting quoted strings with escape sequences.
///
/// Handles `key:"value"` as two tokens: `key:` and the parsed quoted value.
/// Returns tokens where quoted values have escapes processed (\n → newline, \\ → \, \" → ").
fn tokenize_op(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(&ch) = chars.peek() {
        if ch.is_whitespace() {
            chars.next();
            continue;
        }

        if ch == '"' {
            // Standalone quoted string
            tokens.push(parse_quoted_string(&mut chars));
        } else {
            // Unquoted token — but may contain key:"value" pattern
            let mut s = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                if c == '"' {
                    // Hit a quote mid-token: split into key: prefix and quoted value
                    // e.g., old:"foo bar" → token "old:" + token "foo bar"
                    tokens.push(s);
                    tokens.push(parse_quoted_string(&mut chars));
                    s = String::new();
                    break;
                }
                s.push(c);
                chars.next();
            }
            if !s.is_empty() {
                tokens.push(s);
            }
        }
    }

    tokens
}

/// Parse a quoted string starting at the current position (which must be `"`).
/// Processes escape sequences: \n, \t, \\, \".
fn parse_quoted_string(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    chars.next(); // consume opening quote
    let mut s = String::new();
    while let Some(&c) = chars.peek() {
        if c == '\\' {
            chars.next();
            match chars.peek() {
                Some(&'n') => { s.push('\n'); chars.next(); }
                Some(&'t') => { s.push('\t'); chars.next(); }
                Some(&'\\') => { s.push('\\'); chars.next(); }
                Some(&'"') => { s.push('"'); chars.next(); }
                _ => { s.push('\\'); } // keep backslash if not a known escape
            }
        } else if c == '"' {
            chars.next(); // consume closing quote
            break;
        } else {
            s.push(c);
            chars.next();
        }
    }
    s
}

/// Parse an op DSL string into an OpDsl.
///
/// Grammar:
/// ```text
/// str_replace PATH old:"TEXT" new:"TEXT" [replace_all]
/// read PATH [start:N] [end:N] [count:N]
/// write PATH start:N end:N content:"TEXT"
/// cursor PATH to:N
/// ```
///
/// Quoted values support \n (newline), \t (tab), \\ (backslash), \" (quote) escapes.
pub fn parse_op(input: &str) -> Result<OpDsl, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("empty op string".to_string());
    }

    let tokens = tokenize_op(input);
    if tokens.is_empty() {
        return Err("empty op string".to_string());
    }

    let verb = tokens[0].to_lowercase();

    match verb.as_str() {
        "str_replace" => {
            if tokens.len() < 2 {
                return Err("str_replace requires a file path".to_string());
            }
            let path = tokens[1].clone();

            // Parse key:"value" pairs from remaining tokens
            let mut old_str: Option<String> = None;
            let mut new_str: Option<String> = None;
            let mut replace_all = false;

            let mut i = 2;
            while i < tokens.len() {
                let tok = &tokens[i];
                if tok == "replace_all" {
                    replace_all = true;
                    i += 1;
                } else if let Some(key) = tok.strip_suffix(':') {
                    // key: followed by next token as value
                    let key = key.to_lowercase();
                    if i + 1 >= tokens.len() {
                        return Err(format!("missing value for '{key}:'"));
                    }
                    let val = tokens[i + 1].clone();
                    match key.as_str() {
                        "old" => old_str = Some(val),
                        "new" => new_str = Some(val),
                        _ => return Err(format!("unknown key '{key}' in str_replace")),
                    }
                    i += 2;
                } else if let Some((k, v)) = tok.split_once(':') {
                    // key:value (no space) — value is unquoted
                    match k.to_lowercase().as_str() {
                        "old" => old_str = Some(v.to_string()),
                        "new" => new_str = Some(v.to_string()),
                        _ => return Err(format!("unknown key '{k}' in str_replace")),
                    }
                    i += 1;
                } else {
                    return Err(format!("unexpected token '{tok}' in str_replace"));
                }
            }

            Ok(OpDsl::StrReplace {
                path,
                old_str: old_str.ok_or("str_replace requires old:\"...\"")?,
                new_str: new_str.ok_or("str_replace requires new:\"...\"")?,
                replace_all,
            })
        }
        "read" => {
            if tokens.len() < 2 {
                return Err("read requires a file path".to_string());
            }
            let path = tokens[1].clone();
            let kwargs = parse_kwargs_from_tokens(&tokens[2..]);
            let start = parse_opt_usize(&kwargs, "start")?;
            let end = parse_opt_usize(&kwargs, "end")?;
            let count = parse_opt_usize(&kwargs, "count")?;
            Ok(OpDsl::Read { path, start, end, count })
        }
        "write" => {
            if tokens.len() < 2 {
                return Err("write requires a file path".to_string());
            }
            let path = tokens[1].clone();

            let mut start: Option<usize> = None;
            let mut end: Option<usize> = None;
            let mut content: Option<String> = None;

            let mut i = 2;
            while i < tokens.len() {
                let tok = &tokens[i];
                if let Some(key) = tok.strip_suffix(':') {
                    let key = key.to_lowercase();
                    if i + 1 >= tokens.len() {
                        return Err(format!("missing value for '{key}:'"));
                    }
                    let val = &tokens[i + 1];
                    match key.as_str() {
                        "start" => start = Some(val.parse::<usize>().map_err(|_| "invalid start")?),
                        "end" => end = Some(val.parse::<usize>().map_err(|_| "invalid end")?),
                        "content" => content = Some(val.clone()),
                        _ => return Err(format!("unknown key '{key}' in write")),
                    }
                    i += 2;
                } else if let Some((k, v)) = tok.split_once(':') {
                    match k.to_lowercase().as_str() {
                        "start" => start = Some(v.parse::<usize>().map_err(|_| "invalid start")?),
                        "end" => end = Some(v.parse::<usize>().map_err(|_| "invalid end")?),
                        "content" => content = Some(v.to_string()),
                        _ => return Err(format!("unknown key '{k}' in write")),
                    }
                    i += 1;
                } else {
                    return Err(format!("unexpected token '{tok}' in write"));
                }
            }

            Ok(OpDsl::Write {
                path,
                start: start.ok_or("write requires start:N")?,
                end: end.ok_or("write requires end:N")?,
                content: content.ok_or("write requires content:\"...\"")?,
            })
        }
        "cursor" => {
            if tokens.len() < 2 {
                return Err("cursor requires a file path".to_string());
            }
            let path = tokens[1].clone();
            let kwargs = parse_kwargs_from_tokens(&tokens[2..]);
            let to = parse_opt_usize(&kwargs, "to")?
                .ok_or("cursor requires to:N")?;
            Ok(OpDsl::CursorMove { path, to })
        }
        other => Err(format!(
            "unknown op '{other}'. Expected: str_replace, read, write, cursor"
        )),
    }
}

/// Parse key:value pairs from tokenized strings (handles both "key:value" and separated "key:" "value").
fn parse_kwargs_from_tokens(tokens: &[String]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut i = 0;
    while i < tokens.len() {
        let tok = &tokens[i];
        if let Some(key) = tok.strip_suffix(':') {
            // "key:" followed by value token
            if i + 1 < tokens.len() {
                map.insert(key.to_lowercase(), tokens[i + 1].clone());
                i += 2;
                continue;
            }
        }
        if let Some((k, v)) = tok.split_once(':') {
            if !k.is_empty() && !v.is_empty() {
                map.insert(k.to_lowercase(), v.to_string());
            }
        }
        i += 1;
    }
    map
}

fn parse_opt_usize(kwargs: &HashMap<String, String>, key: &str) -> Result<Option<usize>, String> {
    kwargs
        .get(key)
        .map(|v| v.parse::<usize>())
        .transpose()
        .map_err(|_| format!("invalid {key} value"))
}

/// Normalize a mixed DSL/JSON ops array into daemon wire format.
///
/// Each element in the input array can be:
/// - A JSON string → parsed as DSL (e.g. `"str_replace f.rs old:\"foo\" new:\"bar\""`)
/// - A JSON object → validated to have a `"method"` field, passed through
///
/// Returns a JSON array where every element is a daemon-wire-format JSON object
/// with `"method"`, `"path"`, and operation-specific fields.
pub fn normalize_ops(ops: &serde_json::Value) -> Result<serde_json::Value, String> {
    let arr = ops
        .as_array()
        .ok_or_else(|| "ops must be a JSON array".to_string())?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        match item {
            serde_json::Value::String(dsl) => match parse_op(dsl) {
                Ok(op) => out.push(op.to_json()),
                Err(e) => return Err(format!("op {i}: {e}")),
            },
            serde_json::Value::Object(_) => {
                if !item.get("method").and_then(|v| v.as_str()).is_some() {
                    return Err(format!(
                        "op {i}: JSON object must have a \"method\" string field"
                    ));
                }
                let mut op = item.clone();
                // Normalize file.write: accept string content → split to array
                if op.get("method").and_then(|v| v.as_str()) == Some("file.write") {
                    if let Some(serde_json::Value::String(s)) = op.get("content") {
                        let lines: Vec<&str> = s.lines().collect();
                        op["content"] = serde_json::json!(lines);
                    }
                }
                out.push(op);
            }
            _ => return Err(format!("op {i}: expected string (DSL) or object (JSON)")),
        }
    }
    Ok(serde_json::Value::Array(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- SessionAction parse tests ---

    #[test]
    fn open_single_file() {
        let a = parse_session_action("open src/main.rs").unwrap();
        assert_eq!(
            a,
            SessionAction::Open {
                files: vec!["src/main.rs".into()],
                name: None,
            }
        );
    }

    #[test]
    fn open_multiple_files() {
        let a = parse_session_action("open src/a.rs src/b.rs src/c.rs").unwrap();
        assert_eq!(
            a,
            SessionAction::Open {
                files: vec!["src/a.rs".into(), "src/b.rs".into(), "src/c.rs".into()],
                name: None,
            }
        );
    }

    #[test]
    fn open_with_name() {
        let a = parse_session_action("open data.csv as:worker-1").unwrap();
        assert_eq!(
            a,
            SessionAction::Open {
                files: vec!["data.csv".into()],
                name: Some("worker-1".into()),
            }
        );
    }

    #[test]
    fn open_multiple_with_name() {
        let a = parse_session_action("open a.rs b.rs as:agent-2").unwrap();
        assert_eq!(
            a,
            SessionAction::Open {
                files: vec!["a.rs".into(), "b.rs".into()],
                name: Some("agent-2".into()),
            }
        );
    }

    #[test]
    fn open_no_files_error() {
        assert!(parse_session_action("open").is_err());
    }

    #[test]
    fn open_only_kwargs_error() {
        assert!(parse_session_action("open as:worker-1").is_err());
    }

    #[test]
    fn flush_default() {
        let a = parse_session_action("flush").unwrap();
        assert_eq!(
            a,
            SessionAction::Flush {
                name: None,
                force: false,
            }
        );
    }

    #[test]
    fn flush_force() {
        let a = parse_session_action("flush --force").unwrap();
        assert_eq!(
            a,
            SessionAction::Flush {
                name: None,
                force: true,
            }
        );
    }

    #[test]
    fn flush_named() {
        let a = parse_session_action("flush session:worker-1").unwrap();
        assert_eq!(
            a,
            SessionAction::Flush {
                name: Some("worker-1".into()),
                force: false,
            }
        );
    }

    #[test]
    fn flush_named_force() {
        let a = parse_session_action("flush --force session:w1").unwrap();
        assert_eq!(
            a,
            SessionAction::Flush {
                name: Some("w1".into()),
                force: true,
            }
        );
    }

    #[test]
    fn close_default() {
        let a = parse_session_action("close").unwrap();
        assert_eq!(a, SessionAction::Close { name: None, flush: true, force: false });
    }

    #[test]
    fn close_named() {
        let a = parse_session_action("close session:worker-1").unwrap();
        assert_eq!(
            a,
            SessionAction::Close {
                name: Some("worker-1".into()),
                flush: true,
                force: false,
            }
        );
    }

    #[test]
    fn close_no_flush() {
        let a = parse_session_action("close --no-flush").unwrap();
        assert_eq!(a, SessionAction::Close { name: None, flush: false, force: false });
    }

    #[test]
    fn close_force() {
        let a = parse_session_action("close --force").unwrap();
        assert_eq!(a, SessionAction::Close { name: None, flush: true, force: true });
    }

    #[test]
    fn close_force_named() {
        let a = parse_session_action("close --force session:w1").unwrap();
        assert_eq!(a, SessionAction::Close { name: Some("w1".into()), flush: true, force: true });
    }

    // --- Query verbs in parse_session_action ---

    #[test]
    fn session_action_read() {
        let a = parse_session_action("read src/main.rs start:10 end:20").unwrap();
        assert_eq!(a, SessionAction::Read {
            path: "src/main.rs".into(),
            session: None,
            start: Some(10),
            end: Some(20),
            count: None,
        });
    }

    #[test]
    fn session_action_status() {
        assert_eq!(parse_session_action("status").unwrap(), SessionAction::Status);
    }

    #[test]
    fn session_action_list() {
        assert_eq!(parse_session_action("list").unwrap(), SessionAction::List);
    }

    #[test]
    fn session_action_check() {
        assert_eq!(
            parse_session_action("check build").unwrap(),
            SessionAction::Check { action: "build".into() }
        );
    }

    #[test]
    fn register_ok() {
        let a = parse_session_action("register /path/file.xlsx sheets").unwrap();
        assert_eq!(
            a,
            SessionAction::Register {
                path: "/path/file.xlsx".into(),
                handler: "sheets".into(),
            }
        );
    }

    #[test]
    fn register_missing_handler() {
        assert!(parse_session_action("register /path/file.xlsx").is_err());
    }

    #[test]
    fn unregister_ok() {
        let a = parse_session_action("unregister ext-001").unwrap();
        assert_eq!(
            a,
            SessionAction::Unregister {
                tracking_id: "ext-001".into(),
            }
        );
    }

    #[test]
    fn unregister_missing_id() {
        assert!(parse_session_action("unregister").is_err());
    }

    #[test]
    fn empty_action_error() {
        assert!(parse_session_action("").is_err());
        assert!(parse_session_action("  ").is_err());
    }

    #[test]
    fn unknown_action_error() {
        let err = parse_session_action("delete all").unwrap_err();
        assert!(err.contains("unknown action"));
    }

    #[test]
    fn case_insensitive_action() {
        let a = parse_session_action("OPEN src/main.rs").unwrap();
        assert!(matches!(a, SessionAction::Open { .. }));
    }

    // --- Query parse tests ---

    #[test]
    fn read_full_file() {
        let q = parse_query("read src/main.rs").unwrap();
        assert_eq!(
            q,
            Query::Read {
                path: "src/main.rs".into(),
                session: None,
                start: None,
                end: None,
                count: None,
            }
        );
    }

    #[test]
    fn read_range() {
        let q = parse_query("read src/main.rs start:10 end:20").unwrap();
        assert_eq!(
            q,
            Query::Read {
                path: "src/main.rs".into(),
                session: None,
                start: Some(10),
                end: Some(20),
                count: None,
            }
        );
    }

    #[test]
    fn read_cursor() {
        let q = parse_query("read src/main.rs count:50").unwrap();
        assert_eq!(
            q,
            Query::Read {
                path: "src/main.rs".into(),
                session: None,
                start: None,
                end: None,
                count: Some(50),
            }
        );
    }

    #[test]
    fn read_named_session() {
        let q = parse_query("read src/main.rs session:worker-1").unwrap();
        assert_eq!(
            q,
            Query::Read {
                path: "src/main.rs".into(),
                session: Some("worker-1".into()),
                start: None,
                end: None,
                count: None,
            }
        );
    }

    #[test]
    fn read_range_and_session() {
        let q = parse_query("read f.rs start:5 end:10 session:w1").unwrap();
        assert_eq!(
            q,
            Query::Read {
                path: "f.rs".into(),
                session: Some("w1".into()),
                start: Some(5),
                end: Some(10),
                count: None,
            }
        );
    }

    #[test]
    fn read_no_path_error() {
        assert!(parse_query("read").is_err());
    }

    #[test]
    fn read_bad_start() {
        assert!(parse_query("read f.rs start:abc").is_err());
    }

    #[test]
    fn status_query() {
        assert_eq!(parse_query("status").unwrap(), Query::Status);
    }

    #[test]
    fn list_query() {
        assert_eq!(parse_query("list").unwrap(), Query::List);
    }

    #[test]
    fn check_build() {
        assert_eq!(
            parse_query("check build").unwrap(),
            Query::Check {
                action: "build".into(),
            }
        );
    }

    #[test]
    fn check_no_action_error() {
        assert!(parse_query("check").is_err());
    }

    #[test]
    fn empty_query_error() {
        assert!(parse_query("").is_err());
    }

    #[test]
    fn unknown_query_error() {
        let err = parse_query("delete").unwrap_err();
        assert!(err.contains("unknown query"));
    }

    #[test]
    fn case_insensitive_query() {
        assert_eq!(parse_query("STATUS").unwrap(), Query::Status);
        assert_eq!(parse_query("LIST").unwrap(), Query::List);
    }

    // --- OpDsl parse tests ---

    #[test]
    fn op_str_replace_basic() {
        let op = parse_op(r#"str_replace src/main.rs old:"dispatch_op" new:"execute_op""#).unwrap();
        assert_eq!(op, OpDsl::StrReplace {
            path: "src/main.rs".into(),
            old_str: "dispatch_op".into(),
            new_str: "execute_op".into(),
            replace_all: false,
        });
    }

    #[test]
    fn op_str_replace_multiline() {
        let op = parse_op(r#"str_replace f.rs old:"fn foo(\n    x: i32\n)" new:"fn bar(\n    x: i64\n)""#).unwrap();
        assert_eq!(op, OpDsl::StrReplace {
            path: "f.rs".into(),
            old_str: "fn foo(\n    x: i32\n)".into(),
            new_str: "fn bar(\n    x: i64\n)".into(),
            replace_all: false,
        });
    }

    #[test]
    fn op_str_replace_all() {
        let op = parse_op(r#"str_replace f.rs old:"foo" new:"bar" replace_all"#).unwrap();
        assert_eq!(op, OpDsl::StrReplace {
            path: "f.rs".into(),
            old_str: "foo".into(),
            new_str: "bar".into(),
            replace_all: true,
        });
    }

    #[test]
    fn op_read_full() {
        let op = parse_op("read src/main.rs").unwrap();
        assert_eq!(op, OpDsl::Read {
            path: "src/main.rs".into(),
            start: None, end: None, count: None,
        });
    }

    #[test]
    fn op_read_range() {
        let op = parse_op("read src/main.rs start:10 end:20").unwrap();
        assert_eq!(op, OpDsl::Read {
            path: "src/main.rs".into(),
            start: Some(10), end: Some(20), count: None,
        });
    }

    #[test]
    fn op_read_count() {
        let op = parse_op("read f.rs count:50").unwrap();
        assert_eq!(op, OpDsl::Read {
            path: "f.rs".into(),
            start: None, end: None, count: Some(50),
        });
    }

    #[test]
    fn op_write_basic() {
        let op = parse_op(r#"write src/main.rs start:0 end:0 content:"// header\n// line 2""#).unwrap();
        assert_eq!(op, OpDsl::Write {
            path: "src/main.rs".into(),
            start: 0, end: 0,
            content: "// header\n// line 2".into(),
        });
    }

    #[test]
    fn op_cursor_move() {
        let op = parse_op("cursor f.rs to:50").unwrap();
        assert_eq!(op, OpDsl::CursorMove {
            path: "f.rs".into(),
            to: 50,
        });
    }

    #[test]
    fn op_unknown_verb() {
        assert!(parse_op("delete f.rs").is_err());
    }

    #[test]
    fn op_empty() {
        assert!(parse_op("").is_err());
    }

    #[test]
    fn op_str_replace_missing_new() {
        assert!(parse_op(r#"str_replace f.rs old:"foo""#).is_err());
    }

    #[test]
    fn op_to_json_str_replace() {
        let op = OpDsl::StrReplace {
            path: "f.rs".into(),
            old_str: "foo".into(),
            new_str: "bar".into(),
            replace_all: false,
        };
        let j = op.to_json();
        assert_eq!(j["method"], "file.str_replace");
        assert_eq!(j["path"], "f.rs");
        assert_eq!(j["old_str"], "foo");
        assert_eq!(j["new_str"], "bar");
    }

    #[test]
    fn op_to_json_write() {
        let op = OpDsl::Write {
            path: "f.rs".into(),
            start: 0, end: 0,
            content: "line1\nline2".into(),
        };
        let j = op.to_json();
        assert_eq!(j["method"], "file.write");
        assert_eq!(j["content"], serde_json::json!(["line1", "line2"]));
    }

    #[test]
    fn tokenize_escaped_quotes() {
        let tokens = tokenize_op(r#"str_replace f.rs old:"say \"hello\"" new:"say \"bye\"""#);
        // old:"say \"hello\"" → ["old:", "say \"hello\""]
        assert_eq!(tokens[2], "old:");
        assert_eq!(tokens[3], "say \"hello\"");
        assert_eq!(tokens[4], "new:");
        assert_eq!(tokens[5], "say \"bye\"");
    }

    #[test]
    fn op_str_replace_unquoted_simple() {
        // Simple values without spaces can use key:value syntax
        let op = parse_op("str_replace f.rs old:foo new:bar").unwrap();
        assert_eq!(op, OpDsl::StrReplace {
            path: "f.rs".into(),
            old_str: "foo".into(),
            new_str: "bar".into(),
            replace_all: false,
        });
    }

    // --- normalize_ops tests ---

    #[test]
    fn normalize_dsl_strings() {
        let input = serde_json::json!([
            r#"str_replace f.rs old:"foo" new:"bar""#,
            r#"read f.rs start:10 end:20"#,
        ]);
        let result = normalize_ops(&input).unwrap();
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["method"], "file.str_replace");
        assert_eq!(arr[0]["old_str"], "foo");
        assert_eq!(arr[0]["new_str"], "bar");
        assert_eq!(arr[1]["method"], "file.read");
        assert_eq!(arr[1]["start"], 10);
    }

    #[test]
    fn normalize_json_objects() {
        let input = serde_json::json!([
            {"method": "file.write", "path": "f.rs", "start": 0, "end": 0, "content": ["line1"]},
        ]);
        let result = normalize_ops(&input).unwrap();
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["method"], "file.write");
        assert_eq!(arr[0]["content"], serde_json::json!(["line1"]));
    }

    #[test]
    fn normalize_mixed_dsl_and_json() {
        let input = serde_json::json!([
            r#"str_replace f.rs old:"foo" new:"bar" replace_all"#,
            {"method": "file.write", "path": "f.rs", "start": 0, "end": 0, "content": ["x"]},
        ]);
        let result = normalize_ops(&input).unwrap();
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["method"], "file.str_replace");
        assert!(arr[0]["replace_all"].as_bool().unwrap());
        assert_eq!(arr[1]["method"], "file.write");
    }

    #[test]
    fn normalize_rejects_json_without_method() {
        let input = serde_json::json!([
            {"path": "f.rs", "old_str": "foo", "new_str": "bar"},
        ]);
        let err = normalize_ops(&input).unwrap_err();
        assert!(err.contains("method"));
    }

    #[test]
    fn normalize_rejects_non_array() {
        let input = serde_json::json!({"method": "file.read"});
        let err = normalize_ops(&input).unwrap_err();
        assert!(err.contains("array"));
    }

    #[test]
    fn normalize_rejects_bad_element_type() {
        let input = serde_json::json!([42]);
        let err = normalize_ops(&input).unwrap_err();
        assert!(err.contains("expected string"));
    }

    #[test]
    fn normalize_reports_dsl_parse_errors() {
        let input = serde_json::json!(["unknown_verb f.rs"]);
        let err = normalize_ops(&input).unwrap_err();
        assert!(err.contains("op 0"));
    }
}
