use std::io::{self, Write};
use std::path::PathBuf;

use crate::shim::common::{self, ShimError};

pub fn run(args: &[String]) -> i32 {
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(ParseResult::Fallback) => return fallback(args),
        Err(ParseResult::Error(msg)) => {
            eprintln!("sed: {msg}");
            return 2;
        }
    };

    match parsed {
        SedAction::InPlaceSubstitute { subs, files, backup_suffix } => {
            common::run_with_fallback("sed", args, || {
                let rt = common::build_runtime();
                rt.block_on(sed_substitute(&files, &subs, backup_suffix.as_deref()))
            })
        }
        SedAction::PrintRange { start, end, file } => {
            common::run_with_fallback("sed", args, || {
                let rt = common::build_runtime();
                rt.block_on(sed_print_range(&file, start, end))
            })
        }
    }
}

#[derive(Debug)]
enum SedAction {
    InPlaceSubstitute {
        subs: Vec<Substitution>,
        files: Vec<PathBuf>,
        backup_suffix: Option<String>,
    },
    PrintRange {
        start: usize,
        end: usize,
        file: PathBuf,
    },
}

#[derive(Debug, Clone)]
struct Substitution {
    old_str: String,
    new_str: String,
    #[allow(dead_code)]
    global: bool,
}

#[derive(Debug)]
enum ParseResult {
    Fallback,
    Error(String),
}

fn parse_args(args: &[String]) -> Result<SedAction, ParseResult> {
    if args.is_empty() {
        return Err(ParseResult::Fallback);
    }

    let mut in_place = false;
    let mut backup_suffix: Option<String> = None;
    let mut silent = false;
    let mut expressions: Vec<String> = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    let mut has_e_flag = false;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "-i" {
            in_place = true;
            if i + 1 < args.len() {
                let next = &args[i + 1];
                if next.is_empty() {
                    i += 1;
                } else if !next.starts_with('-') && !next.starts_with('s') && !next.contains('/') {
                    backup_suffix = Some(next.clone());
                    i += 1;
                }
            }
        } else if arg.starts_with("-i") && arg.len() > 2 {
            in_place = true;
            backup_suffix = Some(arg[2..].to_string());
        } else if arg == "-n" {
            silent = true;
        } else if arg == "-e" {
            has_e_flag = true;
            i += 1;
            if i >= args.len() {
                return Err(ParseResult::Error("option requires an argument -- e".into()));
            }
            expressions.push(args[i].clone());
        } else if arg == "-f" {
            return Err(ParseResult::Fallback);
        } else if arg.starts_with('-') && arg.len() > 1 && arg != "--" {
            return Err(ParseResult::Fallback);
        } else if arg == "--" {
            files.extend(args[i + 1..].iter().map(|a| PathBuf::from(a)));
            break;
        } else if expressions.is_empty() && !has_e_flag && files.is_empty() {
            expressions.push(arg.clone());
        } else {
            files.push(PathBuf::from(arg));
        }
        i += 1;
    }

    if expressions.len() > 1 {
        return Err(ParseResult::Fallback);
    }

    if expressions.is_empty() {
        return Err(ParseResult::Fallback);
    }

    let expr = &expressions[0];

    if silent && !in_place {
        if let Some(action) = try_parse_range_print(expr, &files) {
            return Ok(action);
        }
    }

    if in_place {
        if let Some(sub) = parse_substitution(expr) {
            if files.is_empty() {
                return Err(ParseResult::Error("no input files".into()));
            }
            return Ok(SedAction::InPlaceSubstitute {
                subs: vec![sub],
                files,
                backup_suffix,
            });
        }
        return Err(ParseResult::Fallback);
    }

    Err(ParseResult::Fallback)
}

fn try_parse_range_print(expr: &str, files: &[PathBuf]) -> Option<SedAction> {
    let expr = expr.strip_suffix('p')?;
    let (start_str, end_str) = expr.split_once(',')?;
    let start: usize = start_str.trim().parse().ok()?;
    let end: usize = end_str.trim().parse().ok()?;
    if files.len() != 1 {
        return None;
    }
    Some(SedAction::PrintRange {
        start,
        end,
        file: files[0].clone(),
    })
}

fn parse_substitution(expr: &str) -> Option<Substitution> {
    if !expr.starts_with('s') || expr.len() < 4 {
        return None;
    }
    let delim = expr.as_bytes()[1] as char;
    let rest = &expr[2..];
    let (old_raw, remainder) = split_unescaped(rest, delim)?;
    let (new_raw, flags_str) = split_unescaped(&remainder, delim)?;

    let old_str = unescape_sed(&old_raw);
    let new_str = unescape_sed_replacement(&new_raw);

    let global = flags_str.contains('g');

    for ch in flags_str.chars() {
        match ch {
            'g' | 'I' | 'i' => {}
            _ => return None,
        }
    }

    Some(Substitution { old_str, new_str, global })
}

fn split_unescaped(s: &str, delim: char) -> Option<(String, String)> {
    let mut result = String::new();
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            result.push('\\');
            if let Some(next) = chars.next() {
                result.push(next);
            }
        } else if ch == delim {
            return Some((result, chars.collect()));
        } else {
            result.push(ch);
        }
    }
    None
}

fn unescape_sed(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => result.push('\n'),
                Some('t') => result.push('\t'),
                Some('\\') => result.push('\\'),
                Some(c) => {
                    result.push(c);
                }
                None => result.push('\\'),
            }
        } else {
            result.push(ch);
        }
    }
    result
}

fn unescape_sed_replacement(s: &str) -> String {
    unescape_sed(s)
}

async fn sed_substitute(
    files: &[PathBuf],
    subs: &[Substitution],
    backup_suffix: Option<&str>,
) -> Result<(), ShimError> {
    let mut client = common::connect().await?;
    let session_id = common::session_open(&mut client, files).await?;

    for file in files {
        if let Some(suffix) = backup_suffix {
            let backup_path = format!("{}{}", file.display(), suffix);
            let (lines, _, trailing_newline) =
                common::file_read_all(&mut client, &session_id, file).await?;
            let backup = PathBuf::from(&backup_path);
            let mut content = lines.join("\n");
            if trailing_newline {
                content.push('\n');
            }
            std::fs::write(&backup, content).map_err(ShimError::Io)?;
        }

        for sub in subs {
            let result = common::file_str_replace(
                &mut client,
                &session_id,
                file,
                &sub.old_str,
                &sub.new_str,
                true,
            )
            .await;

            match result {
                Ok(_) => {}
                Err(slipstream_core::client::ClientError::Rpc { message, .. })
                    if message.contains("no match found") =>
                {
                    // No match → silent no-op (matches real sed behavior)
                }
                Err(e) => {
                    let _ = close_no_flush(&mut client, &session_id).await;
                    return Err(ShimError::Client(e));
                }
            }
        }
    }

    common::session_close(&mut client, &session_id).await?;
    Ok(())
}

async fn sed_print_range(file: &PathBuf, start: usize, end: usize) -> Result<(), ShimError> {
    let mut client = common::connect().await?;
    let session_id = common::session_open(&mut client, &[file.clone()]).await?;

    // sed uses 1-indexed lines, daemon uses 0-indexed
    let (lines, trailing_newline) = common::file_read(
        &mut client,
        &session_id,
        file,
        Some(start.saturating_sub(1)),
        Some(end),
    )
    .await?;

    let stdout = io::stdout();
    let mut out = stdout.lock();

    // Check if we're reading to the end of the file
    let (_, total, _) = common::file_read_all(&mut client, &session_id, file).await?;
    let at_eof = end >= total;
    let line_count = lines.len();

    for (i, line) in lines.iter().enumerate() {
        out.write_all(line.as_bytes()).map_err(ShimError::Io)?;
        let is_final = i == line_count - 1;
        if !(is_final && at_eof && !trailing_newline) {
            out.write_all(b"\n").map_err(ShimError::Io)?;
        }
    }

    common::session_close(&mut client, &session_id).await?;
    Ok(())
}

async fn close_no_flush(
    client: &mut slipstream_core::client::Client,
    session_id: &str,
) -> Result<(), slipstream_core::client::ClientError> {
    client
        .request(
            "session.close",
            serde_json::json!({
                "session_id": session_id,
                "flush": false,
            }),
        )
        .await?;
    Ok(())
}

fn fallback(args: &[String]) -> i32 {
    common::run_with_fallback("sed", args, || Err(ShimError::Fallback))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_substitution() {
        let sub = parse_substitution("s/foo/bar/").unwrap();
        assert_eq!(sub.old_str, "foo");
        assert_eq!(sub.new_str, "bar");
        assert!(!sub.global);
    }

    #[test]
    fn parse_global_substitution() {
        let sub = parse_substitution("s/foo/bar/g").unwrap();
        assert!(sub.global);
    }

    #[test]
    fn parse_alternate_delimiter() {
        let sub = parse_substitution("s|foo|bar|").unwrap();
        assert_eq!(sub.old_str, "foo");
        assert_eq!(sub.new_str, "bar");
    }

    #[test]
    fn parse_hash_delimiter() {
        let sub = parse_substitution("s#old#new#g").unwrap();
        assert_eq!(sub.old_str, "old");
        assert_eq!(sub.new_str, "new");
        assert!(sub.global);
    }

    #[test]
    fn parse_escaped_delimiter() {
        let sub = parse_substitution(r"s/foo\/bar/baz/").unwrap();
        assert_eq!(sub.old_str, "foo/bar");
        assert_eq!(sub.new_str, "baz");
    }

    #[test]
    fn parse_escaped_newline() {
        let sub = parse_substitution(r"s/foo/bar\nbaz/").unwrap();
        assert_eq!(sub.new_str, "bar\nbaz");
    }

    #[test]
    fn parse_escaped_tab() {
        let sub = parse_substitution(r"s/foo/bar\tbaz/").unwrap();
        assert_eq!(sub.new_str, "bar\tbaz");
    }

    #[test]
    fn parse_escaped_backslash() {
        let sub = parse_substitution(r"s/foo/bar\\baz/").unwrap();
        assert_eq!(sub.new_str, "bar\\baz");
    }

    #[test]
    fn parse_empty_replacement() {
        let sub = parse_substitution("s/foo//").unwrap();
        assert_eq!(sub.old_str, "foo");
        assert_eq!(sub.new_str, "");
    }

    #[test]
    fn split_unescaped_basic() {
        let (before, after) = split_unescaped("foo/bar/", '/').unwrap();
        assert_eq!(before, "foo");
        assert_eq!(after, "bar/");
    }

    #[test]
    fn split_unescaped_with_escape() {
        let (before, after) = split_unescaped(r"foo\/bar/baz/", '/').unwrap();
        assert_eq!(before, r"foo\/bar");
        assert_eq!(after, "baz/");
    }

    #[test]
    fn range_print_parse() {
        let files = vec![PathBuf::from("file.txt")];
        let action = try_parse_range_print("10,20p", &files).unwrap();
        match action {
            SedAction::PrintRange { start, end, .. } => {
                assert_eq!(start, 10);
                assert_eq!(end, 20);
            }
            _ => panic!("expected PrintRange"),
        }
    }

    #[test]
    fn parse_macos_inplace() {
        let args: Vec<String> = vec![
            "-i".into(),
            "".into(),
            "s/old/new/".into(),
            "file.txt".into(),
        ];
        let result = parse_args(&args);
        assert!(result.is_ok());
        match result.unwrap() {
            SedAction::InPlaceSubstitute { subs, files, backup_suffix } => {
                assert_eq!(subs[0].old_str, "old");
                assert_eq!(subs[0].new_str, "new");
                assert_eq!(files, vec![PathBuf::from("file.txt")]);
                assert!(backup_suffix.is_none());
            }
            _ => panic!("expected InPlaceSubstitute"),
        }
    }

    #[test]
    fn parse_gnu_inplace() {
        let args: Vec<String> = vec!["-i".into(), "s/old/new/".into(), "file.txt".into()];
        let result = parse_args(&args);
        assert!(result.is_ok());
    }

    #[test]
    fn parse_backup_suffix() {
        let args: Vec<String> = vec!["-i.bak".into(), "s/old/new/".into(), "file.txt".into()];
        let result = parse_args(&args);
        assert!(result.is_ok());
        match result.unwrap() {
            SedAction::InPlaceSubstitute { backup_suffix, .. } => {
                assert_eq!(backup_suffix, Some(".bak".into()));
            }
            _ => panic!("expected InPlaceSubstitute"),
        }
    }

    #[test]
    fn stream_mode_falls_back() {
        let args: Vec<String> = vec!["s/old/new/".into(), "file.txt".into()];
        let result = parse_args(&args);
        assert!(matches!(result, Err(ParseResult::Fallback)));
    }
}
