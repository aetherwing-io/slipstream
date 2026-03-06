mod cat;
mod common;
mod head;
mod sed;
mod tail;

/// Dispatch a shim command by binary name.
/// Returns the process exit code (0 = success).
pub fn dispatch(binary_name: &str, args: &[String]) -> i32 {
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
