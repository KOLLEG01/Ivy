//! Preserve provider payloads while exposing only canonical actionable failures.
use ivy_contracts::{IvyError, IvyJson, IvyResult, Outcome, json::parse};
use ivy_sdk::rpc::response_value;

pub fn tool_result(value: &IvyJson) -> IvyResult<IvyJson> {
    let mut payload = value
        .get("structuredContent")
        .filter(|v| !v.is_null())
        .cloned();
    if payload.is_none() {
        if let Some(items) = value.get("content").and_then(IvyJson::as_array) {
            for entry in items {
                if entry.get("type") == Some(&"text".into()) {
                    if let Some(text) = entry.get("text").and_then(IvyJson::as_js_string) {
                        // JSON.parse consumes UTF-16. Escaped JSON strings remain
                        // lossless; literal unpaired units inside strings are
                        // escaped before passing through the UTF-8 parser.
                        let mut source = String::new();
                        let mut escaped = false;
                        let mut invalid_escape = false;
                        for c in char::decode_utf16(text.0.iter().copied()) {
                            match c {
                                Ok(c) => {
                                    source.push(c);
                                    escaped = c == '\\' && !escaped;
                                }
                                Err(e) => {
                                    if escaped {
                                        invalid_escape = true;
                                        break;
                                    }
                                    source.push_str(&format!("\\u{:04x}", e.unpaired_surrogate()));
                                }
                            }
                        }
                        if !invalid_escape {
                            if let Ok(parsed) = parse(&source, usize::MAX) {
                                payload = Some(parsed);
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
    if value.get("isError") == Some(&true.into()) {
        if let Some(error) = payload.as_ref().and_then(|p| p.get("error")) {
            if let Err(failure) = response_value(
                &IvyJson::object([
                    ("jsonrpc", "2.0".into()),
                    ("id", "mcp-tool".into()),
                    ("error", error.clone()),
                ]),
                "mcp-tool",
            ) {
                let code = &failure.code;
                if code != "invalid_frame"
                    && !code.is_empty()
                    && code.len() <= 128
                    && code.bytes().enumerate().all(|(i, b)| {
                        b.is_ascii_lowercase() || (i > 0 && (b.is_ascii_digit() || b == b'_'))
                    })
                {
                    return Err(IvyError::new(code, "The MCP tool returned an error.")
                        .with_outcome(failure.outcome));
                }
            }
        }
        return Err(
            IvyError::new("provider_unavailable", "The MCP tool returned an error.")
                .with_outcome(Outcome::Unknown),
        );
    }
    Ok(payload.unwrap_or_else(|| value.clone()))
}
