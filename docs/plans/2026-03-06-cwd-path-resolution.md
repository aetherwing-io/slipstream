# CWD Path Resolution Fix

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Canonicalize all file paths to absolute before sending them to the daemon, so relative paths resolve against the client's CWD — not the daemon's.

**Architecture:** Add a `resolve_path()` helper in `slipstream-core` and a `resolve_ops_paths()` function for ops arrays. Apply at both MCP and CLI entry points before paths hit the wire. Use `std::env::current_dir().join(path)` (not `std::fs::canonicalize()` which requires the file to exist).

**Tech Stack:** Rust, slipstream-core, slipstream-mcp, slipstream-cli

---

### Task 1: Add `resolve_path()` helper to slipstream-core

**Files:**
- Modify: `crates/slipstream-core/src/lib.rs`

**Step 1: Write the test**

Add to `crates/slipstream-core/src/lib.rs` (or a new test module):

```rust
#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn resolve_path_absolute_unchanged() {
        let result = resolve_path("/usr/bin/cat");
        assert_eq!(result, "/usr/bin/cat");
    }

    #[test]
    fn resolve_path_relative_becomes_absolute() {
        let result = resolve_path("src/main.rs");
        assert!(result.starts_with('/'), "should be absolute: {result}");
        assert!(result.ends_with("src/main.rs"), "should end with original: {result}");
    }

    #[test]
    fn resolve_path_dot_relative() {
        let result = resolve_path("./Cargo.toml");
        assert!(result.starts_with('/'), "should be absolute: {result}");
        assert!(result.ends_with("Cargo.toml"), "should end with filename: {result}");
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --package slipstream-core path_tests -v`
Expected: FAIL — `resolve_path` not found

**Step 3: Write implementation**

Add to `crates/slipstream-core/src/lib.rs`:

```rust
/// Resolve a path to absolute using the current working directory.
/// Absolute paths pass through unchanged. Relative paths are joined
/// with `std::env::current_dir()`. Falls back to the original path
/// if CWD cannot be determined.
///
/// Uses join (not canonicalize) so non-existent files still resolve.
pub fn resolve_path(path: &str) -> String {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return path.to_string();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(p).to_string_lossy().to_string(),
        Err(_) => path.to_string(),
    }
}
```

**Step 4: Run tests**

Run: `cargo test --package slipstream-core path_tests -v`
Expected: PASS

**Step 5: Commit**

```
git add crates/slipstream-core/src/lib.rs
git commit -m "feat: add resolve_path() helper for CWD-relative path resolution"
```

---

### Task 2: Add `resolve_ops_paths()` for batch ops

**Files:**
- Modify: `crates/slipstream-core/src/lib.rs`

**Step 1: Write the test**

Add to the `path_tests` module:

```rust
#[test]
fn resolve_ops_paths_makes_relative_absolute() {
    let mut ops = serde_json::json!([
        {"method": "file.str_replace", "path": "src/main.rs", "old_str": "a", "new_str": "b"},
        {"method": "file.read", "path": "/absolute/path.rs"},
    ]);
    resolve_ops_paths(&mut ops);
    let arr = ops.as_array().unwrap();
    let p0 = arr[0]["path"].as_str().unwrap();
    let p1 = arr[1]["path"].as_str().unwrap();
    assert!(p0.starts_with('/'), "relative should become absolute: {p0}");
    assert!(p0.ends_with("src/main.rs"));
    assert_eq!(p1, "/absolute/path.rs", "absolute unchanged");
}

#[test]
fn resolve_ops_paths_no_path_field_ok() {
    let mut ops = serde_json::json!([{"method": "session.list"}]);
    resolve_ops_paths(&mut ops); // should not panic
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --package slipstream-core path_tests -v`
Expected: FAIL — `resolve_ops_paths` not found

**Step 3: Write implementation**

Add to `crates/slipstream-core/src/lib.rs`:

```rust
/// Resolve all `"path"` fields in a JSON ops array to absolute paths.
pub fn resolve_ops_paths(ops: &mut serde_json::Value) {
    if let Some(arr) = ops.as_array_mut() {
        for op in arr {
            if let Some(path_val) = op.get("path").and_then(|p| p.as_str()).map(|s| s.to_string()) {
                op["path"] = serde_json::Value::String(resolve_path(&path_val));
            }
        }
    }
}
```

**Step 4: Run tests**

Run: `cargo test --package slipstream-core path_tests -v`
Expected: PASS

**Step 5: Commit**

```
git add crates/slipstream-core/src/lib.rs
git commit -m "feat: add resolve_ops_paths() for batch op path resolution"
```

---

### Task 3: Apply path resolution in MCP server

**Files:**
- Modify: `crates/slipstream-mcp/src/server.rs`

Paths enter the MCP server at these points:
1. `ss()` quick mode — `p.path` (line 239)
2. `ss()` batch mode — paths inside ops (line 295)
3. `ss_session()` Read — `path` (line 392-405)
4. `ss_session()` Open — `files` (line 319-323)
5. `ss_session()` Register — `path` (line 368)

**Step 1: Add the import**

At the top of `server.rs`, the import `use slipstream_core::...` already exists. Add `resolve_path` and `resolve_ops_paths`:

```rust
use slipstream_core::{resolve_path, resolve_ops_paths};
```

**Step 2: Apply to `ss()` quick mode**

In the `ss` method, after extracting `path` from params (line ~239), resolve it before use. Change:

```rust
if let Some(ref path) = p.path {
```

to build a resolved path and use it throughout:

```rust
if let Some(ref raw_path) = p.path {
    let path = resolve_path(raw_path);
```

Then update the three places that use `path` in this block (the two ops constructions and the `files` vec).

**Step 3: Apply to `ss()` batch mode**

After `parse_ops()` (line ~284), resolve paths in the normalized ops:

```rust
let mut json_ops = match parse_ops(items) {
    Ok(ops) => ops,
    Err(msg) => return err_result(msg),
};
resolve_ops_paths(&mut json_ops);
```

**Step 4: Apply to `ss_session()` Read**

In the `SessionAction::Read` match arm (line ~392), resolve the path:

```rust
SessionAction::Read { path, session, start, end, count } => {
    let path = resolve_path(&path);
```

**Step 5: Apply to `ss_session()` Open**

In the `SessionAction::Open` match arm (line ~319), resolve each file:

```rust
SessionAction::Open { files, name } => {
    let files: Vec<String> = files.iter().map(|f| resolve_path(f)).collect();
```

**Step 6: Apply to `ss_session()` Register**

In the `SessionAction::Register` match arm (line ~368):

```rust
SessionAction::Register { path, handler } => {
    let path = resolve_path(&path);
```

**Step 7: Run tests**

Run: `cargo test --workspace -v`
Expected: All existing tests pass (paths in tests are either absolute or resolve correctly in the test CWD).

**Step 8: Commit**

```
git add crates/slipstream-mcp/src/server.rs
git commit -m "fix: resolve relative paths to absolute in MCP server before sending to daemon"
```

---

### Task 4: Apply path resolution in CLI

**Files:**
- Modify: `crates/slipstream-cli/src/main.rs`

Paths enter the CLI at these points:
1. `Command::Open { files }` — `Vec<PathBuf>` (line 293)
2. `Command::Read { path, ... }` — `PathBuf` (line 299-304)
3. `Command::Write { path, ... }` — `PathBuf` (line 339)
4. `Command::Cursor { path, ... }` — `PathBuf` (line 354)
5. `run_exec()` — `files: Vec<PathBuf>` (line 414)
6. `Command::Batch { ops }` — paths inside JSON ops string (line 379)

The simplest approach: resolve `PathBuf` args before converting to `&str`.

**Step 1: Add import**

At top of `main.rs`:

```rust
use slipstream_core::{resolve_path, resolve_ops_paths};
```

Note: `slipstream-cli` depends on `slipstream_core` via `slipstream_cli::client` already, but the `Cargo.toml` may need a direct dependency. Check `crates/slipstream-cli/Cargo.toml` — if `slipstream-core` isn't listed, add it.

**Step 2: Add a helper to resolve PathBuf**

In `main.rs`, add a small helper:

```rust
fn resolve_file(p: &PathBuf) -> PathBuf {
    if p.is_absolute() {
        p.clone()
    } else {
        std::env::current_dir().map(|cwd| cwd.join(p)).unwrap_or_else(|_| p.clone())
    }
}
```

**Step 3: Apply to `run_exec()`**

In `run_exec()` (line ~414), resolve files before converting to strings:

```rust
let files: Vec<PathBuf> = files.into_iter().map(|f| resolve_file(&f)).collect();
```

And resolve ops paths if provided — after `parse_ops()` in step 3 of the function:

```rust
let mut ops_value = parse_ops(&ops_str)?;
resolve_ops_paths(&mut ops_value);
```

**Step 4: Apply to other commands**

In `Command::Open`:
```rust
Command::Open { files } => {
    let files: Vec<PathBuf> = files.into_iter().map(|f| resolve_file(&f)).collect();
```

In `Command::Read` (auto-open path):
```rust
None => {
    let path = resolve_file(&path);
    let path_str = path.to_str().unwrap_or_default();
```

In `Command::Write`, `Command::Cursor`: same pattern — `let path = resolve_file(&path);` before use.

In `Command::Batch`: after `parse_ops()`, apply `resolve_ops_paths(&mut ops_value);`.

**Step 5: Run tests**

Run: `cargo test --workspace`
Expected: All tests pass.

**Step 6: Commit**

```
git add crates/slipstream-cli/src/main.rs
git commit -m "fix: resolve relative paths to absolute in CLI before sending to daemon"
```

---

### Task 5: Version bump and release commit

**Files:**
- Modify: `Cargo.toml` (workspace version)

**Step 1: Bump version**

Change `version = "0.5.10"` to `version = "0.5.11"` in `Cargo.toml`.

**Step 2: Run full test suite**

Run: `cargo test --workspace`
Expected: All tests pass.

**Step 3: Final commit, tag, push**

```
git add -A
git commit -m "fix: CWD-relative path resolution for daemon requests, bump to v0.5.11

Paths sent to the daemon are now resolved to absolute using the client's
working directory. Fixes relative paths resolving against the daemon's CWD
when daemon and client have different working directories (common when MCP
auto-starts the daemon)."

git tag -a v0.5.11 -m "v0.5.11: Fix CWD-relative path resolution"
git push origin main && git push origin v0.5.11
```
