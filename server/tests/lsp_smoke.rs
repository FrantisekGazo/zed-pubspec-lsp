//! Transport-level smoke test: spawns the real binary and drives a framed
//! JSON-RPC session over stdio (initialize → didOpen → hover → shutdown).
//! No network is required — hover on an unknown document position returns null.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

struct Lsp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Lsp {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_pubspec-language-server"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn server");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, message: Value) {
        let body = message.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv(&mut self) -> Value {
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            self.stdout.read_line(&mut line).expect("read header");
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length:") {
                content_length = value.trim().parse().expect("content length");
            }
        }
        let mut body = vec![0u8; content_length];
        self.stdout.read_exact(&mut body).expect("read body");
        serde_json::from_slice(&body).expect("valid json")
    }

    /// Next response with the given id, skipping server-initiated
    /// notifications (e.g. textDocument/publishDiagnostics).
    fn recv_response(&mut self, id: i64) -> Value {
        loop {
            let message = self.recv();
            if message["id"] == json!(id) {
                return message;
            }
        }
    }
}

#[test]
fn initialize_open_hover_shutdown() {
    let mut lsp = Lsp::spawn();

    lsp.send(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "capabilities": {} }
    }));
    let response = lsp.recv_response(1);
    let capabilities = &response["result"]["capabilities"];
    assert_eq!(capabilities["hoverProvider"], json!(true));
    assert_eq!(capabilities["textDocumentSync"], json!(1));
    assert!(capabilities["completionProvider"].is_object());
    assert_eq!(capabilities["codeActionProvider"], json!(true));

    lsp.send(json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
    lsp.send(json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": { "textDocument": {
            "uri": "file:///tmp/pubspec.yaml", "languageId": "yaml",
            "version": 1, "text": "name: demo\n"
        }}
    }));

    // No dependency under the cursor → immediate null, no network involved.
    lsp.send(json!({
        "jsonrpc": "2.0", "id": 2, "method": "textDocument/hover",
        "params": {
            "textDocument": { "uri": "file:///tmp/pubspec.yaml" },
            "position": { "line": 0, "character": 2 }
        }
    }));
    let response = lsp.recv_response(2);
    assert_eq!(response["result"], Value::Null);

    lsp.send(json!({ "jsonrpc": "2.0", "id": 3, "method": "shutdown" }));
    lsp.recv_response(3);
    lsp.send(json!({ "jsonrpc": "2.0", "method": "exit" }));
    let status = lsp.child.wait().expect("wait");
    assert!(status.success());
}
