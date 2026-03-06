use std::io::{self, Read, Write};
use std::path::PathBuf;

use crate::shim::common::{self, ShimError};

pub fn run(args: &[String]) -> i32 {
    let mut number_lines = false;
    let mut files: Vec<PathBuf> = Vec::new();

    for arg in args {
        match arg.as_str() {
            "-n" => number_lines = true,
            "-" => {
                return passthrough_stdin();
            }
            a if a.starts_with('-') && a.len() > 1 => {
                let flags = &a[1..];
                let mut all_known = true;
                for ch in flags.chars() {
                    match ch {
                        'n' => number_lines = true,
                        _ => {
                            all_known = false;
                            break;
                        }
                    }
                }
                if !all_known {
                    return fallback("cat", args);
                }
            }
            _ => files.push(PathBuf::from(arg)),
        }
    }

    if files.is_empty() {
        return passthrough_stdin();
    }

    common::run_with_fallback("cat", args, || {
        let rt = common::build_runtime();
        rt.block_on(cat_files(&files, number_lines))
    })
}

async fn cat_files(files: &[PathBuf], number_lines: bool) -> Result<(), ShimError> {
    let mut client = common::connect().await?;
    let session_id = common::session_open(&mut client, files).await?;

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let file_count = files.len();

    for (file_idx, file) in files.iter().enumerate() {
        let (lines, _total, trailing_newline) =
            common::file_read_all(&mut client, &session_id, file).await?;
        let is_last_file = file_idx == file_count - 1;
        let line_count = lines.len();

        for (i, line) in lines.iter().enumerate() {
            let is_final_line = is_last_file && i == line_count - 1;
            if number_lines {
                let line_num = i + 1;
                write!(out, "{line_num:6}\t{line}").map_err(ShimError::Io)?;
            } else {
                out.write_all(line.as_bytes()).map_err(ShimError::Io)?;
            }
            // Suppress final newline only for the very last line of the last file
            // when the original file had no trailing newline
            if !(is_final_line && !trailing_newline) {
                out.write_all(b"\n").map_err(ShimError::Io)?;
            }
        }
    }

    common::session_close(&mut client, &session_id).await?;
    Ok(())
}

fn passthrough_stdin() -> i32 {
    let mut buf = Vec::new();
    if io::stdin().read_to_end(&mut buf).is_err() {
        return 1;
    }
    if io::stdout().write_all(&buf).is_err() {
        return 1;
    }
    0
}

fn fallback(binary_name: &str, args: &[String]) -> i32 {
    common::run_with_fallback(binary_name, args, || Err(ShimError::Fallback))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_number_flag() {
        let args: Vec<String> = vec!["-n".into(), "file.txt".into()];
        let mut number_lines = false;
        let mut files: Vec<PathBuf> = Vec::new();
        for arg in &args {
            match arg.as_str() {
                "-n" => number_lines = true,
                a if a.starts_with('-') => {}
                _ => files.push(PathBuf::from(arg)),
            }
        }
        assert!(number_lines);
        assert_eq!(files, vec![PathBuf::from("file.txt")]);
    }
}
