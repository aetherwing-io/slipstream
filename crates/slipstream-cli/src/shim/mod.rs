mod cat;
mod common;
mod head;
mod sed;
mod tail;

use std::io::IsTerminal;

/// Dispatch a shim command by binary name.
/// Returns the process exit code (0 = success).
pub fn dispatch(binary_name: &str, args: &[String]) -> i32 {
    // Rule 1: non-TTY stdout → passthrough to real binary.
    // Slipstream's output is for human terminals; pipes, redirects, and
    // subshell captures must get byte-identical output from the real tool.
    if should_passthrough_non_tty() {
        common::fallback_exec(binary_name, args);
    }

    // Rule 2: sed -i → passthrough to real sed.
    // In-place edits need full GNU sed semantics (regex, atomicity, backup
    // suffixes). The shim does literal string matching which silently
    // differs for regex patterns used by build scripts and test runners.
    if binary_name == "sed" && is_sed_inplace(args) {
        common::fallback_exec(binary_name, args);
    }

    match binary_name {
        "cat" => cat::run(args),
        "head" => head::run(args),
        "tail" => tail::run(args),
        "sed" => sed::run(args),
        _ => {
            eprintln!("slipstream shim: unknown command: {binary_name}");
            2
        }
    }
}

/// Returns true when stdout is not a terminal (pipe, redirect, capture).
fn should_passthrough_non_tty() -> bool {
    !std::io::stdout().is_terminal()
}

/// Returns true when args contain -i or -i<suffix> (sed in-place mode).
fn is_sed_inplace(args: &[String]) -> bool {
    args.iter().any(|a| a == "-i" || a.starts_with("-i"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sed_inplace_bare() {
        let args = vec!["-i".into(), "s/a/b/".into(), "f.txt".into()];
        assert!(is_sed_inplace(&args));
    }

    #[test]
    fn sed_inplace_with_suffix() {
        let args = vec!["-i.bak".into(), "s/a/b/".into(), "f.txt".into()];
        assert!(is_sed_inplace(&args));
    }

    #[test]
    fn sed_inplace_empty_suffix() {
        // macOS form: sed -i '' 's/a/b/' f.txt
        // -i is still present as its own arg
        let args = vec!["-i".into(), "".into(), "s/a/b/".into(), "f.txt".into()];
        assert!(is_sed_inplace(&args));
    }

    #[test]
    fn sed_no_inplace() {
        let args = vec!["s/a/b/".into(), "f.txt".into()];
        assert!(!is_sed_inplace(&args));
    }

    #[test]
    fn sed_n_not_inplace() {
        let args = vec!["-n".into(), "5,10p".into(), "f.txt".into()];
        assert!(!is_sed_inplace(&args));
    }
}
