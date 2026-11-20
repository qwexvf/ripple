//! The binary's argument contract, exercised through the real executable.
//!
//! These belong outside `main.rs` because what they assert is the process:
//! what it prints, what it exits with, and — the bug that prompted them — what it
//! writes to disk when asked for help.

use std::process::Command;

fn ripple(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ripple"))
        .args(args)
        .output()
        .expect("run ripple")
}

#[test]
fn help_prints_usage_and_succeeds() {
    for flag in ["--help", "-h", "help"] {
        let out = ripple(&[flag]);
        assert!(out.status.success(), "{flag} exited {:?}", out.status);
        assert!(
            String::from_utf8_lossy(&out.stdout).starts_with("usage:"),
            "{flag} printed no usage on stdout"
        );
    }
}

#[test]
fn version_prints_the_crate_version() {
    for flag in ["--version", "-V"] {
        let out = ripple(&[flag]);
        assert!(out.status.success(), "{flag} exited {:?}", out.status);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            format!("ripple {}", env!("CARGO_PKG_VERSION")),
            "{flag} printed the wrong version"
        );
    }
}

/// The one that bit: `--help` was dropped by the flag parser, `cmd_index` fell
/// back to indexing ".", and asking a question overwrote the graph of whatever
/// directory you happened to be in.
#[test]
fn asking_a_command_for_help_indexes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.ts"), "export function f() {}\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_ripple"))
        .args(["index", "--help"])
        .current_dir(tmp.path())
        .output()
        .expect("run ripple");

    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("usage:"));
    assert!(
        !tmp.path().join(".ripple").exists(),
        "asking for help wrote an index"
    );
}

#[test]
fn an_unknown_flag_is_an_error_not_a_default() {
    let out = ripple(&["index", "--jsonn"]);
    assert!(!out.status.success(), "an unknown flag must not succeed");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unknown flag: --jsonn"),
        "the error must name the flag"
    );
}

/// An ambiguous name used to print one JSON array per match, back to back, which
/// no JSON parser accepts (#130). The caveat goes to stderr and into the document.
#[test]
fn ambiguous_neighbors_json_is_one_document() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir(&src).unwrap();
    std::fs::write(
        src.join("a.ts"),
        "export function handle() { return 1; }\nexport function go() { return handle(); }\n",
    )
    .unwrap();
    std::fs::write(src.join("b.ts"), "export function handle() { return 2; }\n").unwrap();
    let root = tmp.path().to_str().unwrap();
    assert!(ripple(&["index", root]).status.success());

    let out = ripple(&["neighbors", "handle", "--in", "--json", "--root", root]);
    assert!(out.status.success());
    let doc: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout is one JSON document");
    assert_eq!(doc["lookup"]["candidates"], 2);
    assert_eq!(doc["lookup"]["ambiguous"], true);
    assert!(String::from_utf8_lossy(&out.stderr).contains("matches 2 symbols"));

    let human = ripple(&["path", "go", "handl", "--root", root]);
    assert!(
        !String::from_utf8_lossy(&human.stdout).contains("no exact match"),
        "the lookup caveat belongs on stderr"
    );
}
