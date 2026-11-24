//! Sanitized diagnostic tracing hooks for Phase 1 VoIP live-wire verification.
//!
//! Guarantees that authentication secrets (`zpw_sek`, `zpw_enk`, cookies, passwords, sealed
//! tokens) are scrubbed before any trace line is emitted.

use std::fmt;
use std::sync::Arc;

use serde_json::{Map, Value};

/// Thread-safe cloneable callback receiving sanitized single-line diagnostic trace messages.
#[derive(Clone)]
pub struct TraceSink(Arc<dyn Fn(&str) + Send + Sync>);

impl TraceSink {
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(&str) + Send + Sync + 'static,
    {
        Self(Arc::new(f))
    }

    pub fn emit(&self, msg: &str) {
        (self.0)(msg);
    }
}

impl fmt::Debug for TraceSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TraceSink(<enabled>)")
    }
}

/// Emit a formatted trace line only when `sink` is `Some`.
pub fn emit_trace(sink: Option<&TraceSink>, msg: impl FnOnce() -> String) {
    if let Some(s) = sink {
        s.emit(&msg());
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.contains("zpw_sek")
        || lower.contains("zpw_enk")
        || lower.contains("cookie")
        || lower.contains("secret")
        || lower.contains("password")
        || lower.contains("authorization")
        || lower.contains("sealed")
        || lower == "sek"
        || lower == "enk"
}

/// Recursively scrub any sensitive key from a JSON value before logging in `--trace` mode.
pub fn sanitize_trace_json(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            for (k, val) in map {
                if is_sensitive_key(k) {
                    out.insert(k.clone(), Value::String("[REDACTED]".to_string()));
                } else {
                    out.insert(k.clone(), sanitize_trace_json(val));
                }
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(sanitize_trace_json).collect()),
        other => other.clone(),
    }
}

/// Format a sanitized JSON value as a compact single-line string truncated to `max_chars`.
pub fn format_trace_json(v: &Value, max_chars: usize) -> String {
    let sanitized = sanitize_trace_json(v);
    let raw = sanitized.to_string();
    if raw.chars().count() <= max_chars {
        raw
    } else {
        let mut s: String = raw.chars().take(max_chars).collect();
        s.push_str("…(truncated)");
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sanitize_trace_json_redacts_secrets_recursively_while_preserving_voip_fields() {
        let raw = json!({
            "callId": 464564007,
            "zpw_sek": "TOP_SECRET_SEK",
            "zpw_enk": "TOP_SECRET_ENK",
            "Cookie": "zpw_sek=TOP_SECRET_SEK; _zlang=vi",
            "nested": {
                "secret_key": "deadbeef",
                "authorization": "Bearer xyz",
                "sealed_zpw_sek": "dpapi_blob",
                "servers": [
                    { "rtpaddr": "42.113.206.4:10490", "password": "pw" }
                ]
            }
        });
        let formatted = format_trace_json(&raw, 4096);
        assert!(!formatted.contains("TOP_SECRET_SEK"));
        assert!(!formatted.contains("TOP_SECRET_ENK"));
        assert!(!formatted.contains("deadbeef"));
        assert!(!formatted.contains("Bearer xyz"));
        assert!(!formatted.contains("dpapi_blob"));
        assert!(!formatted.contains("\"pw\""));
        assert!(formatted.contains("464564007"));
        assert!(formatted.contains("42.113.206.4:10490"));
        assert!(formatted.contains("[REDACTED]"));
    }
}
