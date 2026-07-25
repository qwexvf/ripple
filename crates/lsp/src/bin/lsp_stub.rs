//! A minimal, scriptable LSP server — exists only so the client's framing,
//! timeout and shutdown paths are testable without installing a real server.
//!
//! The first argument selects the behaviour under test (an argument, not an
//! environment variable: the tests run in parallel inside one process, so shared
//! mutable state would race):
//!   `ok` (default)      full handshake, symbols, call hierarchy
//!   `no-callhierarchy`  capabilities without `callHierarchyProvider`
//!   `hang`              accepts requests, never answers
//!   `garbage`           writes non-LSP bytes instead of a frame
//!   `exit-after-init`   answers `initialize`, then closes stdout
//!   `needs-ack`         asks the client a question and stalls until it answers
//!                       (what tsgo does with `client/registerCapability`)

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "ok".to_owned());
    let mut input = BufReader::new(std::io::stdin());
    let mut out = std::io::stdout();

    // `needs-ack`: after the handshake, ask the client something and refuse to serve
    // anything else until it replies — what tsgo does, and what made it look like a
    // server that never answers.
    let mut awaiting_ack = false;
    // requests that arrive while the ack is outstanding are held, not dropped — a
    // real server queues them, and the client is allowed to pipeline
    let mut held: std::collections::VecDeque<Value> = std::collections::VecDeque::new();
    loop {
        // held messages are only taken once the ack is in — popping them first and
        // putting them back would drop one per iteration
        let msg = if awaiting_ack || held.is_empty() {
            match read_message(&mut input) {
                Some(m) => m,
                None => break,
            }
        } else {
            held.pop_front().expect("checked non-empty")
        };
        if awaiting_ack {
            if msg.get("id").and_then(Value::as_i64) == Some(9001) {
                awaiting_ack = false; // the client answered; serve what was held
            } else {
                held.push_back(msg);
            }
            continue;
        }
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            continue;
        };
        let id = msg.get("id").cloned();
        if mode == "garbage" {
            let _ = out.write_all(b"this is not a jsonrpc frame\n");
            let _ = out.flush();
            continue;
        }
        if mode == "hang" {
            continue; // read everything, answer nothing
        }
        let Some(id) = id else {
            if method == "exit" {
                return;
            }
            continue; // a notification
        };

        let result = match method {
            "initialize" => initialize_result(&mode),
            "textDocument/documentSymbol" => document_symbols(),
            "textDocument/prepareCallHierarchy" => prepare(&msg),
            "callHierarchy/incomingCalls" => incoming_calls(),
            "shutdown" => Value::Null,
            _ => Value::Null,
        };
        write_message(
            &mut out,
            &json!({"jsonrpc": "2.0", "id": id, "result": result}),
        );
        if mode == "exit-after-init" && method == "initialize" {
            return; // drop stdout mid-session
        }
        if mode == "needs-ack" && method == "initialize" {
            write_message(
                &mut out,
                &json!({
                    "jsonrpc": "2.0", "id": 9001,
                    "method": "client/registerCapability",
                    "params": {"registrations": []}
                }),
            );
            awaiting_ack = true;
        }
    }
}

fn initialize_result(mode: &str) -> Value {
    let mut caps = json!({
        "referencesProvider": true,
        "documentSymbolProvider": { "workDoneProgress": true },
        "definitionProvider": true,
    });
    if mode != "no-callhierarchy" {
        caps["callHierarchyProvider"] = json!(true);
    }
    json!({
        "capabilities": caps,
        "serverInfo": { "name": "stub", "version": "1.2.3" },
    })
}

/// Hierarchical `DocumentSymbol`, including a nested method and a class that must
/// be skipped — the client only wants functions and methods.
fn document_symbols() -> Value {
    let range = json!({"start": {"line": 3, "character": 4}, "end": {"line": 9, "character": 1}});
    json!([
        {
            "name": "top_level",
            "kind": 12,
            "range": range,
            "selectionRange": {"start": {"line": 3, "character": 4}, "end": {"line": 3, "character": 13}},
        },
        {
            "name": "Holder",
            "kind": 5,
            "range": range,
            "selectionRange": {"start": {"line": 20, "character": 6}, "end": {"line": 20, "character": 12}},
            "children": [{
                "name": "nested_method",
                "kind": 6,
                "range": range,
                "selectionRange": {"start": {"line": 21, "character": 8}, "end": {"line": 21, "character": 21}},
            }],
        },
    ])
}

/// No item for line 99 — the protocol's way of saying "I don't know this symbol",
/// which the client must not confuse with "no callers".
fn prepare(msg: &Value) -> Value {
    if msg.pointer("/params/position/line").and_then(Value::as_u64) == Some(99) {
        return json!([]);
    }
    json!([{
        "name": "target",
        "kind": 12,
        "uri": "file:///proj/lib/target.ex",
        "range": {"start": {"line": 3, "character": 0}, "end": {"line": 9, "character": 1}},
        "selectionRange": {"start": {"line": 3, "character": 4}, "end": {"line": 3, "character": 10}},
    }])
}

fn incoming_calls() -> Value {
    json!([
        {
            "from": {
                "name": "caller_one",
                "kind": 12,
                "uri": "file:///proj/lib/a.ex",
                "range": {"start": {"line": 10, "character": 0}, "end": {"line": 14, "character": 1}},
                "selectionRange": {"start": {"line": 10, "character": 4}, "end": {"line": 10, "character": 14}},
            },
            // two call sites inside the caller, one repeated — callers must see the
            // call's own position, not just the caller's definition line
            "fromRanges": [
                {"start": {"line": 11, "character": 4}, "end": {"line": 11, "character": 20}},
                {"start": {"line": 11, "character": 4}, "end": {"line": 11, "character": 20}},
                {"start": {"line": 13, "character": 4}, "end": {"line": 13, "character": 20}},
            ],
        },
        {
            "from": {
                "name": "caller_two",
                "kind": 6,
                "uri": "file:///proj/lib/b.ex",
                "range": {"start": {"line": 41, "character": 2}, "end": {"line": 44, "character": 3}},
                "selectionRange": {"start": {"line": 41, "character": 6}, "end": {"line": 41, "character": 16}},
            },
            "fromRanges": [],
        },
        // malformed entry: must be skipped, not fatal
        { "from": { "name": "no_uri" } },
    ])
}

fn write_message(out: &mut impl Write, msg: &Value) {
    let Ok(body) = serde_json::to_vec(msg) else {
        return;
    };
    let _ = write!(out, "Content-Length: {}\r\n\r\n", body.len());
    let _ = out.write_all(&body);
    let _ = out.flush();
}

fn read_message(reader: &mut impl BufRead) -> Option<Value> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            len = v.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; len?];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}
