use std::io::{self, Write};
use std::path::PathBuf;

use crate::shim::common::{self, ShimError};

pub fn run(args: &[String]) -> i32 {
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(()) => return fallback(args),
    };

    if parsed.files.is_empty() || parsed.follow || parsed.bytes_mode {
        return fallback(args);
    }

    common::run_with_fallback("tail", args, || {
        let rt = common::build_runtime();
        rt.block_on(tail_files(&parsed.files, parsed.count, parsed.from_start))
    })
}

struct TailArgs {
    count: usize,
    from_start: bool,
    files: Vec<PathBuf>,
    follow: bool,
    bytes_mode: bool,
}

fn parse_args(args: &[String]) -> Result<TailArgs, ()> {
    let mut count: usize = 10;
    let mut from_start = false;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut follow = false;
    let mut bytes_mode = false;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "-f" || arg == "--follow" {
            follow = true;
        } else if arg == "-n" {
            i += 1;
            if i >= args.len() {
                return Err(());
            }
            let val = &args[i];
            if let Some(rest) = val.strip_prefix('+') {
                from_start = true;
                count = rest.parse().map_err(|_| ())?;
            } else {
                count = val.parse().map_err(|_| ())?;
            }
        } else if arg == "-c" {
            bytes_mode = true;
            i += 1;
        } else if arg.starts_with("-") && arg.len() > 1 && arg[1..].chars().all(|c| c.is_ascii_digit()) {
            count = arg[1..].parse().map_err(|_| ())?;
        } else if arg.starts_with('-') && arg.len() > 1 {
            return Err(());
        } else {
            files.push(PathBuf::from(arg));
        }
        i += 1;
    }

    Ok(TailArgs { count, from_start, files, follow, bytes_mode })
}

async fn tail_files(
    files: &[PathBuf],
    count: usize,
    from_start: bool,
) -> Result<(), ShimError> {
    let mut client = common::connect().await?;
    let session_id = common::session_open(&mut client, files).await?;

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let multi = files.len() > 1;
    let file_count = files.len();

    for (idx, file) in files.iter().enumerate() {
        if multi {
            if idx > 0 {
                out.write_all(b"\n").map_err(ShimError::Io)?;
            }
            writeln!(out, "==> {} <==", file.display()).map_err(ShimError::Io)?;
        }

        let (all_lines, total, trailing_newline) =
            common::file_read_all(&mut client, &session_id, file).await?;

        let start = if from_start {
            if count > 0 { count - 1 } else { 0 }
        } else {
            total.saturating_sub(count)
        };

        let slice = &all_lines[start..];
        let is_last_file = idx == file_count - 1;
        let slice_len = slice.len();

        for (i, line) in slice.iter().enumerate() {
            out.write_all(line.as_bytes()).map_err(ShimError::Io)?;
            let is_final_line = is_last_file && i == slice_len - 1;
            if !(is_final_line && !trailing_newline) {
                out.write_all(b"\n").map_err(ShimError::Io)?;
            }
        }
    }

    common::session_close(&mut client, &session_id).await?;
    Ok(())
}

fn fallback(args: &[String]) -> i32 {
    common::run_with_fallback("tail", args, || Err(ShimError::Fallback))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_default() {
        let args: Vec<String> = vec!["file.txt".into()];
        let parsed = parse_args(&args).unwrap();
        assert_eq!(parsed.count, 10);
        assert!(!parsed.from_start);
        assert!(!parsed.follow);
    }

    #[test]
    fn parse_dash_n() {
        let args: Vec<String> = vec!["-n".into(), "20".into(), "file.txt".into()];
        let parsed = parse_args(&args).unwrap();
        assert_eq!(parsed.count, 20);
        assert!(!parsed.from_start);
    }

    #[test]
    fn parse_from_start() {
        let args: Vec<String> = vec!["-n".into(), "+5".into(), "file.txt".into()];
        let parsed = parse_args(&args).unwrap();
        assert_eq!(parsed.count, 5);
        assert!(parsed.from_start);
    }

    #[test]
    fn parse_short_form() {
        let args: Vec<String> = vec!["-15".into(), "file.txt".into()];
        let parsed = parse_args(&args).unwrap();
        assert_eq!(parsed.count, 15);
    }

    #[test]
    fn parse_follow_flags() {
        let args: Vec<String> = vec!["-f".into(), "file.txt".into()];
        let parsed = parse_args(&args).unwrap();
        assert!(parsed.follow);
    }
}
