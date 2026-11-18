//! Minimal MCP client for the MT5 terminal's local HTTP server.
//!
//! One session, one `call` helper. Deliberately dumb: it speaks just enough
//! JSON-RPC over HTTP to drive the terminal, and knows nothing about trading.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

pub struct Mcp {
    url: String,
    token: String,
    sid: Option<String>,
}

impl Mcp {
    pub fn connect(url: &str, token: &str) -> Result<Self> {
        let mut m = Self {
            url: url.to_string(),
            token: token.to_string(),
            sid: None,
        };
        m.initialize()?;
        Ok(m)
    }

    fn initialize(&mut self) -> Result<()> {
        self.post(&json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "xau-live", "version": env!("CARGO_PKG_VERSION") }
            }
        }))?;
        self.post(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))?;
        Ok(())
    }

    fn post(&mut self, body: &Value) -> Result<Value> {
        let mut req = ureq::post(&self.url)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json, text/event-stream")
            .set("Connection", "close")
            .set("Authorization", &format!("Bearer {}", self.token));
        if let Some(sid) = &self.sid {
            req = req.set("Mcp-Session-Id", sid);
        }

        let payload = serde_json::to_string(body)?;
        let resp = match req.send_string(&payload) {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) => {
                let text = r.into_string().unwrap_or_default();
                bail!("HTTP {code}: {}", truncate(&text, 400));
            }
            Err(e) => return Err(e).context("MCP transport"),
        };

        if let Some(sid) = resp.header("Mcp-Session-Id") {
            self.sid = Some(sid.to_string());
        }
        let status = resp.status();
        let ctype = resp.header("Content-Type").unwrap_or("").to_string();
        let text = resp.into_string().context("reading MCP body")?;

        if status != 200 && status != 202 {
            bail!("HTTP {status}: {}", truncate(&text, 400));
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        parse_body(&text, &ctype)
    }

    /// Call a tool and return its decoded JSON payload.
    pub fn call(&mut self, tool: &str, args: Value) -> Result<Value> {
        let body = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": tool, "arguments": args }
        });
        let obj = self.post(&body)?;

        if let Some(err) = obj.get("error") {
            bail!("{tool}: {err}");
        }
        let result = obj.get("result").cloned().unwrap_or(Value::Null);
        let is_err = result.get("isError").and_then(Value::as_bool).unwrap_or(false);

        let mut texts = Vec::new();
        if let Some(arr) = result.get("content").and_then(Value::as_array) {
            for c in arr {
                if c.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(t) = c.get("text").and_then(Value::as_str) {
                        texts.push(t.to_string());
                    }
                }
            }
        }
        let payload = texts.join(" ");

        if is_err {
            bail!("{tool} failed: {}", truncate(&payload, 600));
        }
        if payload.is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&payload).unwrap_or(Value::String(payload)))
    }
}

/// The terminal answers either a bare JSON object or an SSE frame.
fn parse_body(text: &str, ctype: &str) -> Result<Value> {
    let looks_sse = ctype.contains("text/event-stream")
        || text.trim_start().starts_with("event:")
        || text.trim_start().starts_with("data:");
    if !looks_sse {
        return Ok(serde_json::from_str(text).context("MCP body was not JSON")?);
    }
    // Take the last well-formed `data:` payload in the stream.
    let mut last: Option<Value> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            if let Ok(v) = serde_json::from_str::<Value>(rest.trim()) {
                last = Some(v);
            }
        }
    }
    last.context("SSE stream contained no JSON data frame")
}

fn truncate(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}
