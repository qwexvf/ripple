//! Client contract, against the scriptable stub server (`src/bin/lsp_stub.rs`).
//! Covers the paths a real server exercises only by accident: a missing capability,
//! a server that never answers, one that writes garbage, and one that dies
//! mid-session. Phase 2 of docs/11-lsp-integration.md builds on these.

use lsp::{Caps, Client, Health, ServerSpec};
use std::path::{Path, PathBuf};

fn spec(mode: &str) -> ServerSpec {
    // mode travels as an argument, not an env var: these tests run in parallel in
    // one process, and a shared env var raced (three tests failed against the wrong
    // stub behaviour before this)
    ServerSpec {
        language: "elixir".to_owned(),
        command: env!("CARGO_BIN_EXE_lsp_stub").to_owned(),
        args: vec![mode.to_owned()],
        root_markers: Vec::new(), // always applicable
        inline: true,
        max_concurrency: 1,
        init_timeout_ms: 5_000,
        request_timeout_ms: 1_000,
    }
}

/// A real directory: the client spawns the server with the root as its working
/// directory (servers with an on-disk index locate it from cwd), so a fake path
/// fails at spawn time. The stub still reports `/proj/...` uris, which is what the
/// path-mapping assertions check.
fn root() -> PathBuf {
    std::env::temp_dir()
}

fn connect(mode: &str) -> (Client, Caps) {
    let spec = spec(mode);
    let mut client = Client::start(&spec, &root()).expect("stub starts");
    let (caps, server) = client.initialize(&root(), &spec).expect("handshake");
    assert_eq!(server.as_deref(), Some("stub 1.2.3"));
    (client, caps)
}

#[test]
fn handshake_reports_capabilities() {
    let (client, caps) = connect("ok");
    assert!(caps.call_hierarchy);
    assert!(caps.references);
    assert!(caps.document_symbol, "an object provider counts");
    assert!(!caps.workspace_symbol, "absent means unsupported");
    assert!(caps.usable_for_calls());
    client.stop();
}

#[test]
fn a_server_without_call_hierarchy_can_still_serve_references() {
    let (client, caps) = connect("no-callhierarchy");
    assert!(!caps.call_hierarchy);
    assert!(caps.usable_for_calls(), "references alone is enough");
    client.stop();
}

#[test]
fn document_symbols_yield_only_functions_and_methods() {
    let (mut client, _) = connect("ok");
    let syms = client
        .functions(Path::new("/proj/lib/target.ex"))
        .expect("symbols");
    let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["top_level", "nested_method"],
        "a class must be skipped and a nested method found"
    );
    // positions come from the server and go straight back — no column arithmetic
    assert_eq!((syms[0].line, syms[0].character), (3, 4));
    assert_eq!((syms[1].line, syms[1].character), (21, 8));
    client.stop();
}

#[test]
fn incoming_calls_map_to_paths_and_skip_malformed_entries() {
    let (mut client, _) = connect("ok");
    let calls = client
        .incoming_calls(Path::new("/proj/lib/target.ex"), 3, 4)
        .expect("request ok")
        .expect("server knows this symbol");

    let seen: Vec<(&str, &str, u32)> = calls
        .iter()
        .map(|c| (c.path.to_str().unwrap_or_default(), c.name.as_str(), c.line))
        .collect();
    assert_eq!(
        seen,
        [
            ("/proj/lib/a.ex", "caller_one", 11),
            ("/proj/lib/b.ex", "caller_two", 42),
        ],
        "uris become paths, lines become 1-based, the entry without a uri is dropped"
    );
    client.stop();
}

/// "I don't know this symbol" and "this symbol has no callers" are different
/// answers, and only one of them counts against ripple in a precision comparison.
#[test]
fn an_unknown_symbol_is_none_not_an_empty_set() {
    let (mut client, _) = connect("ok");
    let calls = client
        .incoming_calls(Path::new("/proj/lib/target.ex"), 99, 0)
        .expect("request ok");
    assert!(calls.is_none());
    client.stop();
}

#[test]
fn a_server_that_never_answers_times_out() {
    let spec = spec("hang");
    let mut client = Client::start(&spec, &root()).expect("stub starts");
    let started = std::time::Instant::now();
    let err = client
        .request(
            "initialize",
            serde_json::json!({}),
            std::time::Duration::from_millis(300),
        )
        .expect_err("must not block forever");
    assert!(err.to_string().contains("timed out"), "got {err}");
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    client.stop();
}

#[test]
fn garbage_on_the_pipe_is_reported_not_parsed() {
    let spec = spec("garbage");
    let mut client = Client::start(&spec, &root()).expect("stub starts");
    let err = client
        .request(
            "initialize",
            serde_json::json!({}),
            std::time::Duration::from_millis(500),
        )
        .expect_err("a non-lsp process must not look like success");
    let msg = err.to_string();
    assert!(
        msg.contains("timed out") || msg.contains("exited"),
        "got {msg}"
    );
    client.stop();
}

/// A dead server is noticed by two different paths depending on timing — the write
/// to its stdin fails, or the reader channel disconnects — so both must produce the
/// same message. Asserting only on "exited" passed by luck before the write path was
/// given context: whichever path won was a race.
#[test]
fn a_server_dying_mid_session_reports_the_same_error_either_way() {
    let spec = spec("exit-after-init");
    let mut client = Client::start(&spec, &root()).expect("stub starts");
    // the handshake must survive the server exiting before the `initialized`
    // notification is written — that write is fire-and-forget, and racing it under
    // load used to fail the handshake with a bare "Broken pipe"
    client.initialize(&root(), &spec).expect("handshake");
    let err = client
        .functions(Path::new("/proj/lib/target.ex"))
        .expect_err("the server is gone");
    let msg = format!("{err:#}");
    assert!(msg.contains("exited"), "got {msg}");
    assert!(
        !msg.to_lowercase().contains("broken pipe") || msg.contains("exited"),
        "a raw io error must not be the whole story: {msg}"
    );
    client.stop();
}

/// `probe` is what `doctor` reports, so its verdicts are part of the contract.
#[test]
fn probe_reports_ready_and_missing() {
    let report = lsp::probe(&spec("ok"), &root());
    match report.health {
        Health::Ready { caps, server, .. } => {
            assert!(caps.call_hierarchy);
            assert_eq!(server.as_deref(), Some("stub 1.2.3"));
        }
        other => panic!("expected Ready, got {other:?}"),
    }

    let mut missing = spec("ok");
    missing.command = "/nonexistent/definitely-not-a-server".to_owned();
    assert!(matches!(
        lsp::probe(&missing, &root()).health,
        Health::BinaryMissing
    ));

    // a real file that isn't executable is not a server either
    let not_exec = std::env::temp_dir().join(format!("ripple-not-exec-{}", std::process::id()));
    std::fs::write(&not_exec, "#!/bin/sh\n").expect("write");
    let mut fake = spec("ok");
    fake.command = not_exec.display().to_string();
    assert!(
        matches!(lsp::probe(&fake, &root()).health, Health::BinaryMissing),
        "a non-executable file must not be reported as a usable server"
    );
    let _ = std::fs::remove_file(&not_exec);
}
