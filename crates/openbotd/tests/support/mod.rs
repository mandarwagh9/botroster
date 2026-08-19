//! Shared fixtures for the openbotd integration tests.
#![allow(dead_code)] // each test binary uses a different subset

use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A one-file MCP server that answers `tools/list` and `tools/call`, and
/// records every Authorization header it received.
///
/// Hand-rolled rather than mocked: the credential has to survive a real HTTP
/// request for the test to mean anything.
pub async fn mock_mcp(seen: Arc<Mutex<Vec<String>>>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let seen = Arc::clone(&seen);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();

                for line in req.lines() {
                    if let Some(v) = line.to_lowercase().strip_prefix("authorization:") {
                        seen.lock().unwrap().push(v.trim().to_owned());
                    }
                }

                let body = if req.contains("tools/list") {
                    json!({
                        "jsonrpc": "2.0", "id": 1,
                        "result": { "tools": [{
                            "name": "create_issue",
                            "description": "Create an issue",
                            "inputSchema": { "type": "object",
                                "properties": { "title": { "type": "string" } } }
                        }]}
                    })
                } else {
                    json!({
                        "jsonrpc": "2.0", "id": 1,
                        "result": { "content": [{ "type": "text", "text": "issue ROO-1 created" }] }
                    })
                }
                .to_string();

                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(body.as_bytes()).await;
                let _ = sock.flush().await;
            });
        }
    });
    format!("http://{addr}/mcp")
}

/// A server that accepts the connection and then never answers, for testing
/// that a hung connector cannot hang the hub.
pub async fn black_hole() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((sock, _)) = listener.accept().await {
            // Hold the socket open, answer nothing.
            held.push(sock);
        }
    });
    format!("http://{addr}/mcp")
}
