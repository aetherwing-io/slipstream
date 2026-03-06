//! End-to-end tests: shim mode via unified `slipstream` binary vs real system binaries.
//!
//! Starts a single daemon, runs all shim commands via std::process::Command,
//! and diffs output against the real /usr/bin or /bin equivalents.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use slipstream_core::manager::SessionManager;
use slipstream_daemon::coordinator::Coordinator;
use slipstream_daemon::fcp_bridge::FcpBridge;
use slipstream_daemon::plugin_manager::PluginManager;
use slipstream_daemon::registry::FormatRegistry;

// ── Helpers ──

fn find_real(name: &str) -> PathBuf {
    for dir in &["/usr/bin", "/bin", "/usr/local/bin"] {
        let p = PathBuf::from(dir).join(name);
        if p.exists() {
            return p;
        }
    }
    panic!("cannot find real {name} binary");
}

fn start_daemon(socket: &Path) -> tokio::runtime::Runtime {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let mgr = Arc::new(SessionManager::new());
    let registry = Arc::new(FormatRegistry::default_registry());
    let coordinator = Arc::new(Coordinator::new());
    let fcp_bridge = Arc::new(FcpBridge::new());
    let plugin_mgr = Arc::new(PluginManager::new());
    let listener = rt.block_on(async { tokio::net::UnixListener::bind(socket).unwrap() });

    rt.spawn(slipstream_daemon::serve(
        listener,
        mgr,
        registry,
        coordinator,
        fcp_bridge,
        plugin_mgr,
        socket.to_path_buf(),
    ));

    rt
}

/// Run slipstream in shim mode using direct-invocation: `slipstream` with first arg as command name.
/// Since we can't change argv[0] easily, we rely on the direct dispatch in shim::dispatch
/// by having the binary invoked as `slipstream` — BUT the shim dispatch only triggers on argv[0].
/// So instead we use a symlink approach via the test helper.
fn shim(socket: &Path, name: &str, args: &[&str]) -> (String, i32) {
    let bin = slipstream_bin();
    // Create a temp symlink to simulate argv[0] dispatch
    let link_dir = std::env::temp_dir().join(format!("slipstream-shim-test-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&link_dir);
    let link_path = link_dir.join(name);
    let _ = std::fs::remove_file(&link_path);
    std::os::unix::fs::symlink(&bin, &link_path).unwrap();

    let out = Command::new(&link_path)
        .args(args)
        .env("SLIPSTREAM_SOCKET", socket)
        .env("SLIPSTREAM_SHIM_FALLBACK_DIR", find_real(name).parent().unwrap())
        .output()
        .unwrap();

    let _ = std::fs::remove_file(&link_path);

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let code = out.status.code().unwrap_or(-1);
    (stdout, code)
}

fn slipstream_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_slipstream"))
}

/// Run a real system binary, return stdout.
fn real(name: &str, args: &[&str]) -> String {
    let out = Command::new(find_real(name)).args(args).output().unwrap();
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Build platform-correct sed -i args (macOS needs `-i ''`, Linux needs `-i`).
fn platform_sed_i_args(args: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for &arg in args {
        if arg == "-i" {
            out.push("-i".into());
            if cfg!(target_os = "macos") {
                out.push(String::new());
            }
        } else {
            out.push(arg.into());
        }
    }
    out
}

/// Run shim sed -i (mutates file, no stdout to compare).
/// With non-TTY passthrough, this execs real sed — args must be platform-correct.
fn shim_sed_i(socket: &Path, args: &[&str]) {
    let bin = slipstream_bin();
    let link_dir = std::env::temp_dir().join(format!("slipstream-shim-test-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&link_dir);
    let link_path = link_dir.join("sed");
    let _ = std::fs::remove_file(&link_path);
    std::os::unix::fs::symlink(&bin, &link_path).unwrap();

    let cmd_args = platform_sed_i_args(args);
    let out = Command::new(&link_path)
        .args(&cmd_args)
        .env("SLIPSTREAM_SOCKET", socket)
        .env("SLIPSTREAM_SHIM_FALLBACK_DIR", find_real("sed").parent().unwrap())
        .output()
        .unwrap();

    let _ = std::fs::remove_file(&link_path);

    assert!(
        out.status.success(),
        "shim sed failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Run real sed -i on macOS (needs -i '') or Linux (needs -i).
fn real_sed_i(args: &[&str], file: &Path) {
    let real_sed = find_real("sed");
    let cmd_args = platform_sed_i_args(args);
    let out = Command::new(&real_sed)
        .args(&cmd_args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "real sed failed on {}: {}",
        file.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write_file(path: &Path, content: &str) {
    std::fs::write(path, content).unwrap();
}

fn read_file(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

// ── The single test ──

#[test]
fn shim_vs_native() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("test.sock");
    let _rt = start_daemon(&socket);

    std::thread::sleep(std::time::Duration::from_millis(100));

    // Create test file (20 lines, trailing newline)
    let f = dir.path().join("example.py");
    write_file(
        &f,
        "def hello():\n\
         \x20   print(\"Hello, world!\")\n\
         \n\
         def add(a, b):\n\
         \x20   return a + b\n\
         \n\
         def multiply(x, y):\n\
         \x20   return x * y\n\
         \n\
         class Calculator:\n\
         \x20   def __init__(self):\n\
         \x20       self.history = []\n\
         \n\
         \x20   def compute(self, op, a, b):\n\
         \x20       if op == \"add\":\n\
         \x20           result = add(a, b)\n\
         \x20       elif op == \"multiply\":\n\
         \x20           result = multiply(a, b)\n\
         \x20       self.history.append(result)\n\
         \x20       return result\n",
    );
    let fp = f.to_str().unwrap();

    let mut pass = 0u32;
    let mut fail = 0u32;

    macro_rules! check {
        ($label:expr, $shim_out:expr, $real_out:expr) => {
            if $shim_out == $real_out {
                pass += 1;
            } else {
                eprintln!("FAIL: {}", $label);
                eprintln!("  shim: {:?}", &$shim_out[..80.min($shim_out.len())]);
                eprintln!("  real: {:?}", &$real_out[..80.min($real_out.len())]);
                fail += 1;
            }
        };
    }

    macro_rules! check_file {
        ($label:expr, $path_a:expr, $path_b:expr) => {
            let a = read_file($path_a);
            let b = read_file($path_b);
            if a == b {
                pass += 1;
            } else {
                eprintln!("FAIL: {}", $label);
                eprintln!("  shim file: {:?}", &a[..80.min(a.len())]);
                eprintln!("  real file: {:?}", &b[..80.min(b.len())]);
                fail += 1;
            }
        };
    }

    macro_rules! check_eq {
        ($label:expr, $actual:expr, $expected:expr) => {
            if $actual == $expected {
                pass += 1;
            } else {
                eprintln!("FAIL: {}", $label);
                eprintln!("  actual:   {:?}", $actual);
                eprintln!("  expected: {:?}", $expected);
                fail += 1;
            }
        };
    }

    // ── cat ──

    let (s, _) = shim(&socket, "cat", &[fp]);
    let r = real("cat", &[fp]);
    check!("cat <file>", s, r);

    let (s, _) = shim(&socket, "cat", &["-n", fp]);
    let r = real("cat", &["-n", fp]);
    check!("cat -n <file>", s, r);

    let f1 = dir.path().join("f1.txt");
    let f2 = dir.path().join("f2.txt");
    write_file(&f1, "file1 line1\nfile1 line2\n");
    write_file(&f2, "file2 line1\nfile2 line2\n");
    let (s, _) = shim(&socket, "cat", &[f1.to_str().unwrap(), f2.to_str().unwrap()]);
    let r = real("cat", &[f1.to_str().unwrap(), f2.to_str().unwrap()]);
    check!("cat <f1> <f2> (multi-file)", s, r);

    // ── head ──

    let (s, _) = shim(&socket, "head", &[fp]);
    let r = real("head", &[fp]);
    check!("head <file> (default 10)", s, r);

    let (s, _) = shim(&socket, "head", &["-n", "5", fp]);
    let r = real("head", &["-n", "5", fp]);
    check!("head -n 5", s, r);

    let (s, _) = shim(&socket, "head", &["-3", fp]);
    let r = real("head", &["-3", fp]);
    check!("head -3 (short form)", s, r);

    let (s, _) = shim(&socket, "head", &["-n", "1", fp]);
    let r = real("head", &["-n", "1", fp]);
    check!("head -n 1 (single line)", s, r);

    let (s, _) = shim(&socket, "head", &["-n", "100", fp]);
    let r = real("head", &["-n", "100", fp]);
    check!("head -n 100 (exceeds file)", s, r);

    // ── tail ──

    let (s, _) = shim(&socket, "tail", &[fp]);
    let r = real("tail", &[fp]);
    check!("tail <file> (default 10)", s, r);

    let (s, _) = shim(&socket, "tail", &["-n", "5", fp]);
    let r = real("tail", &["-n", "5", fp]);
    check!("tail -n 5", s, r);

    let (s, _) = shim(&socket, "tail", &["-3", fp]);
    let r = real("tail", &["-3", fp]);
    check!("tail -3 (short form)", s, r);

    let (s, _) = shim(&socket, "tail", &["-n", "1", fp]);
    let r = real("tail", &["-n", "1", fp]);
    check!("tail -n 1 (single line)", s, r);

    let (s, _) = shim(&socket, "tail", &["-n", "+5", fp]);
    let r = real("tail", &["-n", "+5", fp]);
    check!("tail -n +5 (from line 5)", s, r);

    let (s, _) = shim(&socket, "tail", &["-n", "+1", fp]);
    let r = real("tail", &["-n", "+1", fp]);
    check!("tail -n +1 (entire file)", s, r);

    let (s, _) = shim(&socket, "tail", &["-n", "+15", fp]);
    let r = real("tail", &["-n", "+15", fp]);
    check!("tail -n +15 (near end)", s, r);

    let (s, _) = shim(&socket, "tail", &["-n", "100", fp]);
    let r = real("tail", &["-n", "100", fp]);
    check!("tail -n 100 (exceeds file)", s, r);

    // ── sed -i (in-place substitution) ──

    let ss = dir.path().join("sed1s.py");
    let sr = dir.path().join("sed1r.py");
    write_file(&ss, &read_file(&f));
    write_file(&sr, &read_file(&f));
    shim_sed_i(&socket, &["-i", "s/Calculator/Calc/", ss.to_str().unwrap()]);
    real_sed_i(&["-i", "s/Calculator/Calc/", sr.to_str().unwrap()], &sr);
    check_file!("sed -i single match", &ss, &sr);

    let ss = dir.path().join("sed2s.py");
    let sr = dir.path().join("sed2r.py");
    write_file(&ss, &read_file(&f));
    write_file(&sr, &read_file(&f));
    shim_sed_i(&socket, &["-i", "s/result/answer/g", ss.to_str().unwrap()]);
    real_sed_i(&["-i", "s/result/answer/g", sr.to_str().unwrap()], &sr);
    check_file!("sed -i global multi-match", &ss, &sr);

    let ss = dir.path().join("sed3s.py");
    let sr = dir.path().join("sed3r.py");
    write_file(&ss, &read_file(&f));
    write_file(&sr, &read_file(&f));
    shim_sed_i(&socket, &["-i", "s/hello/greet/", ss.to_str().unwrap()]);
    real_sed_i(&["-i", "s/hello/greet/", sr.to_str().unwrap()], &sr);
    check_file!("sed -i passthrough", &ss, &sr);

    let ss = dir.path().join("sed4s.py");
    let sr = dir.path().join("sed4r.py");
    write_file(&ss, &read_file(&f));
    write_file(&sr, &read_file(&f));
    shim_sed_i(
        &socket,
        &["-i", r#"s|"Hello, world!"|"Greetings!"|"#, ss.to_str().unwrap()],
    );
    real_sed_i(
        &["-i", r#"s|"Hello, world!"|"Greetings!"|"#, sr.to_str().unwrap()],
        &sr,
    );
    check_file!("sed -i alternate delimiter", &ss, &sr);

    let ss = dir.path().join("sed5s.py");
    write_file(&ss, &read_file(&f));
    let before = read_file(&ss);
    shim_sed_i(&socket, &["-i", "s/NONEXISTENT/X/", ss.to_str().unwrap()]);
    check_eq!("sed -i zero matches file unchanged", read_file(&ss), before);

    let ss = dir.path().join("sed6s.py");
    write_file(&ss, &read_file(&f));
    shim_sed_i(&socket, &["-i.bak", "s/hello/greet/", ss.to_str().unwrap()]);
    let bak = dir.path().join("sed6s.py.bak");
    check_eq!(
        "sed -i.bak modified",
        read_file(&ss).lines().next().unwrap().to_string(),
        "def greet():".to_string()
    );
    check_eq!(
        "sed -i.bak backup preserved",
        read_file(&bak).lines().next().unwrap().to_string(),
        "def hello():".to_string()
    );

    let m1s = dir.path().join("m1s.py");
    let m2s = dir.path().join("m2s.py");
    let m1r = dir.path().join("m1r.py");
    let m2r = dir.path().join("m2r.py");
    write_file(&m1s, "aaa\n");
    write_file(&m2s, "aaa\n");
    write_file(&m1r, "aaa\n");
    write_file(&m2r, "aaa\n");
    shim_sed_i(
        &socket,
        &["-i", "s/aaa/bbb/", m1s.to_str().unwrap(), m2s.to_str().unwrap()],
    );
    real_sed_i(
        &["-i", "s/aaa/bbb/", m1r.to_str().unwrap(), m2r.to_str().unwrap()],
        &m1r,
    );
    check_file!("sed -i multi-file (file 1)", &m1s, &m1r);
    check_file!("sed -i multi-file (file 2)", &m2s, &m2r);

    // ── sed -i same-line multi-match (daemon bug fix) ──

    let ss = dir.path().join("sml1s.py");
    let sr = dir.path().join("sml1r.py");
    write_file(&ss, "answer and answer\ntwo\nanswer\n");
    write_file(&sr, "answer and answer\ntwo\nanswer\n");
    shim_sed_i(&socket, &["-i", "s/answer/REPLACED/g", ss.to_str().unwrap()]);
    real_sed_i(&["-i", "s/answer/REPLACED/g", sr.to_str().unwrap()], &sr);
    check_file!("sed -i /g same-line 2 matches", &ss, &sr);

    let ss = dir.path().join("sml2s.py");
    let sr = dir.path().join("sml2r.py");
    write_file(&ss, "a-a-a\n");
    write_file(&sr, "a-a-a\n");
    shim_sed_i(&socket, &["-i", "s/a/X/g", ss.to_str().unwrap()]);
    real_sed_i(&["-i", "s/a/X/g", sr.to_str().unwrap()], &sr);
    check_file!("sed -i /g same-line 3 matches", &ss, &sr);

    let ss = dir.path().join("sml3s.py");
    let sr = dir.path().join("sml3r.py");
    write_file(&ss, "a.a.a\n");
    write_file(&sr, "a.a.a\n");
    shim_sed_i(&socket, &["-i", "s/a/XYZ/g", ss.to_str().unwrap()]);
    real_sed_i(&["-i", "s/a/XYZ/g", sr.to_str().unwrap()], &sr);
    check_file!("sed -i /g longer replacement 3 matches", &ss, &sr);

    // ── sed -n (range print) ──

    let (s, _) = shim(&socket, "sed", &["-n", "5,10p", fp]);
    let r = real("sed", &["-n", "5,10p", fp]);
    check!("sed -n '5,10p'", s, r);

    let (s, _) = shim(&socket, "sed", &["-n", "1,3p", fp]);
    let r = real("sed", &["-n", "1,3p", fp]);
    check!("sed -n '1,3p' (first 3)", s, r);

    let (s, _) = shim(&socket, "sed", &["-n", "18,20p", fp]);
    let r = real("sed", &["-n", "18,20p", fp]);
    check!("sed -n '18,20p' (last 3)", s, r);

    // ── sed stream mode (no -i) → fallback ──

    let (s, _) = shim(&socket, "sed", &["s/hello/goodbye/", fp]);
    let r = real("sed", &["s/hello/goodbye/", fp]);
    check!("sed stream mode fallback", s, r);

    // ══════════════════════════════════════════════════════════════════
    // NEW TESTS: trailing newline, empty files, multi-file headers, etc.
    // ══════════════════════════════════════════════════════════════════

    // ── Trailing newline tests ──

    let no_nl = dir.path().join("no_nl.txt");
    write_file(&no_nl, "line1\nline2\nline3");  // no trailing \n
    let no_nl_p = no_nl.to_str().unwrap();

    let (s, _) = shim(&socket, "cat", &[no_nl_p]);
    let r = real("cat", &[no_nl_p]);
    check!("cat no-trailing-newline", s, r);

    let (s, _) = shim(&socket, "head", &["-n", "100", no_nl_p]);
    let r = real("head", &["-n", "100", no_nl_p]);
    check!("head -n 100 no-trailing-newline", s, r);

    let (s, _) = shim(&socket, "tail", &["-n", "2", no_nl_p]);
    let r = real("tail", &["-n", "2", no_nl_p]);
    check!("tail -n 2 no-trailing-newline", s, r);

    // Single-line file with no trailing newline
    let single_no_nl = dir.path().join("single_no_nl.txt");
    write_file(&single_no_nl, "hello");
    let single_no_nl_p = single_no_nl.to_str().unwrap();

    let (s, _) = shim(&socket, "cat", &[single_no_nl_p]);
    let r = real("cat", &[single_no_nl_p]);
    check!("cat single-line no-trailing-newline", s, r);

    // ── Empty file tests ──

    let empty = dir.path().join("empty.txt");
    write_file(&empty, "");
    let empty_p = empty.to_str().unwrap();

    let (s, _) = shim(&socket, "cat", &[empty_p]);
    let r = real("cat", &[empty_p]);
    check!("cat empty file", s, r);

    let (s, _) = shim(&socket, "head", &[empty_p]);
    let r = real("head", &[empty_p]);
    check!("head empty file", s, r);

    let (s, _) = shim(&socket, "tail", &[empty_p]);
    let r = real("tail", &[empty_p]);
    check!("tail empty file", s, r);

    // ── Multi-file headers ──

    let mf1 = dir.path().join("mf1.txt");
    let mf2 = dir.path().join("mf2.txt");
    write_file(&mf1, "alpha\nbeta\ngamma\n");
    write_file(&mf2, "one\ntwo\nthree\n");
    let mf1_p = mf1.to_str().unwrap();
    let mf2_p = mf2.to_str().unwrap();

    let (s, _) = shim(&socket, "head", &["-n", "2", mf1_p, mf2_p]);
    let r = real("head", &["-n", "2", mf1_p, mf2_p]);
    check!("head -n 2 multi-file headers", s, r);

    let (s, _) = shim(&socket, "tail", &["-n", "1", mf1_p, mf2_p]);
    let r = real("tail", &["-n", "1", mf1_p, mf2_p]);
    check!("tail -n 1 multi-file headers", s, r);

    // ── Additional sed tests ──

    // Deletion (empty replacement)
    let del_s = dir.path().join("del_s.txt");
    let del_r = dir.path().join("del_r.txt");
    write_file(&del_s, "foo bar baz\n");
    write_file(&del_r, "foo bar baz\n");
    shim_sed_i(&socket, &["-i", "s/bar //", del_s.to_str().unwrap()]);
    real_sed_i(&["-i", "s/bar //", del_r.to_str().unwrap()], &del_r);
    check_file!("sed -i deletion (empty replacement)", &del_s, &del_r);

    // sed -n single line print (fallback to real sed)
    let (s, _) = shim(&socket, "sed", &["-n", "5p", fp]);
    let r = real("sed", &["-n", "5p", fp]);
    check!("sed -n '5p' (single line, fallback)", s, r);

    // sed -n '$p' (last line, fallback)
    let (s, _) = shim(&socket, "sed", &["-n", "$p", fp]);
    let r = real("sed", &["-n", "$p", fp]);
    check!("sed -n '$p' (last line, fallback)", s, r);

    // ── cat -n edge cases ──

    // Blank lines with numbering
    let blanks = dir.path().join("blanks.txt");
    write_file(&blanks, "one\n\ntwo\n\nthree\n");
    let blanks_p = blanks.to_str().unwrap();

    let (s, _) = shim(&socket, "cat", &["-n", blanks_p]);
    let r = real("cat", &["-n", blanks_p]);
    check!("cat -n blank lines", s, r);

    // Global numbering across files
    let (s, _) = shim(&socket, "cat", &["-n", mf1_p, mf2_p]);
    let r = real("cat", &["-n", mf1_p, mf2_p]);
    check!("cat -n global numbering across files", s, r);

    // ── Bonus ──

    // File with only a newline
    let just_nl = dir.path().join("just_nl.txt");
    write_file(&just_nl, "\n");
    let just_nl_p = just_nl.to_str().unwrap();

    let (s, _) = shim(&socket, "cat", &[just_nl_p]);
    let r = real("cat", &[just_nl_p]);
    check!("cat file with only newline", s, r);

    // cat -n on no-trailing-newline file
    let (s, _) = shim(&socket, "cat", &["-n", no_nl_p]);
    let r = real("cat", &["-n", no_nl_p]);
    check!("cat -n no-trailing-newline", s, r);

    // ── Summary ──

    eprintln!("\n  {pass} passed, {fail} failed\n");
    assert_eq!(fail, 0, "{fail} comparison(s) failed — see details above");
}
