use std::io::{self, Write};
use std::path::PathBuf;

use crate::shim::common::{self, ShimError};

pub fn run(args: &[String]) -> i32 {
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(()) => return fallback(args),
    };

    if parsed.files.is_empty() || parsed.bytes_mode {
        return fallback(args);
    }

    common::run_with_fallback("head", args, || {
        let rt = common::build_runtime();
        rt.block_on(head_files(&parsed.files, parsed.count))
    })
}

struct HeadArgs {
    count: usize,
    files: Vec<PathBuf>,
    bytes_mode: bool,
}

fn parse_args(args: &[String]) -> Result<HeadArgs, ()> {
    let mut count: usize = 10;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut bytes_mode = false;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "-n" {
            i += 1;
            if i >= args.len() {
                return Err(());
            }
            count = args[i].parse().map_err(|_| ())?;
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

    Ok(HeadArgs { count, files, bytes_mode })
}

async fn head_files(files: &[PathBuf], count: usize) -> Result<(), ShimError> {
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
        let (lines, trailing_newline) =
            common::file_read(&mut client, &session_id, file, Some(0), Some(count)).await?;

        let is_last_file = idx == file_count - 1;
        let line_count = lines.len();
        // At EOF if we got fewer lines than requested
        let at_eof = line_count < count;

        for (i, line) in lines.iter().enumerate() {
            out.write_all(line.as_bytes()).map_err(ShimError::Io)?;
            let is_final_line = is_last_file && i == line_count - 1;
            if !(is_final_line && at_eof && !trailing_newline) {
                out.write_all(b"\n").map_err(ShimError::Io)?;
            }
        }
    }

    common::session_close(&mut client, &session_id).await?;
    Ok(())
}

fn fallback(args: &[String]) -> i32 {
    common::run_with_fallback("head", args, || Err(ShimError::Fallback))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_default() {
        let args: Vec<String> = vec!["file.txt".into()];
        let parsed = parse_args(&args).unwrap();
        assert_eq!(parsed.count, 10);
        assert_eq!(parsed.files, vec![PathBuf::from("file.txt")]);
    }

    #[test]
    fn parse_dash_n() {
        let args: Vec<String> = vec!["-n".into(), "50".into(), "file.txt".into()];
        let parsed = parse_args(&args).unwrap();
        assert_eq!(parsed.count, 50);
    }

    #[test]
    fn parse_short_form() {
        let args: Vec<String> = vec!["-20".into(), "file.txt".into()];
        let parsed = parse_args(&args).unwrap();
        assert_eq!(parsed.count, 20);
    }

    #[test]
    fn parse_bytes_mode_flags_fallback() {
        let args: Vec<String> = vec!["-c".into(), "100".into(), "file.txt".into()];
        let parsed = parse_args(&args).unwrap();
        assert!(parsed.bytes_mode);
    }
}
