//! OpenAI-compatible client for llama-server (/v1/chat/completions).

use serde_json::{json, Value};

/// A blocking HTTP client for talking to the local llama-server.
///
/// `.no_proxy()` is essential: llama-server listens on 127.0.0.1, but reqwest
/// auto-detects the Windows system proxy. If a local proxy (e.g. Clash on
/// 127.0.0.1:7890) is enabled, our loopback requests would be routed through it
/// and hang — which previously made the health check time out even though the
/// server was already listening.
pub fn local_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .expect("build blocking client")
}

/// Build the chat-completions request body for a JSON-schema-constrained reply
/// with the model's reasoning phase disabled.
pub fn build_json_body(system: &str, user: &str, schema: Value, max_tokens: u32) -> Value {
    json!({
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ],
        "temperature": 0.1,
        "max_tokens": max_tokens,
        "response_format": {"type": "json_schema", "json_schema": {"name": "out", "schema": schema}},
        "chat_template_kwargs": {"enable_thinking": false}
    })
}

/// Build a vision chat-completions body: a text instruction plus one PNG image
/// (base64 data-URI), JSON-schema-constrained, reasoning disabled.
pub fn build_image_request(
    system: &str,
    user: &str,
    image_png: &[u8],
    schema: Value,
    max_tokens: u32,
) -> Value {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(image_png);
    let data_uri = format!("data:image/png;base64,{b64}");
    json!({
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": [
                {"type": "text", "text": user},
                {"type": "image_url", "image_url": {"url": data_uri}}
            ]}
        ],
        "temperature": 0.1,
        "max_tokens": max_tokens,
        "response_format": {"type": "json_schema", "json_schema": {"name": "out", "schema": schema}},
        "chat_template_kwargs": {"enable_thinking": false}
    })
}

/// POST a chat request and return the assistant message `content`.
pub fn chat(endpoint: &str, body: &Value) -> Result<String, String> {
    let http = local_client();
    let resp = http
        .post(format!("{endpoint}/v1/chat/completions"))
        .json(body)
        .timeout(std::time::Duration::from_secs(240))
        .send()
        .map_err(|e| format!("request: {e}"))?;
    if !resp.status().is_success() {
        let code = resp.status();
        let detail = resp.text().unwrap_or_default();
        return Err(format!("llama-server {code}: {detail}"));
    }
    let v: Value = resp.json().map_err(|e| format!("decode: {e}"))?;
    v["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "no message content".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_has_schema_and_thinking_off() {
        let schema = json!({"type":"object"});
        let b = build_json_body("sys", "usr", schema, 256);
        assert_eq!(b["response_format"]["type"], "json_schema");
        assert_eq!(b["chat_template_kwargs"]["enable_thinking"], false);
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][1]["content"], "usr");
        assert_eq!(b["max_tokens"], 256);
    }

    #[test]
    fn image_request_has_text_and_image_parts() {
        let schema = json!({"type":"object"});
        let b = build_image_request("sys", "describe", &[1u8, 2, 3], schema, 256);
        assert_eq!(b["messages"][0]["role"], "system");
        let content = &b["messages"][1]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "describe");
        assert_eq!(content[1]["type"], "image_url");
        let url = content[1]["image_url"]["url"].as_str().unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
        assert_eq!(b["response_format"]["type"], "json_schema");
        assert_eq!(b["chat_template_kwargs"]["enable_thinking"], false);
        assert_eq!(b["max_tokens"], 256);
    }
}
