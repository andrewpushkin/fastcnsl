//! The JSON of requests and replies, for Anthropic's Messages API and the
//! Chat Completions format (OpenAI, xAI, and servers that copy it). Pure
//! functions: the client in `client.rs` does the HTTP.

use serde_json::{Value, json};

use super::AiError;

/// A tool the model may call.
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON Schema of its input.
    pub schema: Value,
}

/// One question: a system prompt, the user's text and the tools.
pub struct Request<'a> {
    pub model: &'a str,
    pub system: &'a str,
    pub user: &'a str,
    pub tools: &'a [Tool],
    /// Limit on the reply (Messages API only: Chat Completions servers
    /// differ in what they call it, and their defaults are fine).
    pub max_tokens: u32,
}

/// A call the model made to one of the tools.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub name: String,
    pub input: Value,
}

/// What the model answered.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Reply {
    pub text: String,
    pub calls: Vec<ToolCall>,
}

/// The body of `POST /v1/messages`. No sampling settings, no thinking
/// settings and no forced tool: current Claude models reject them.
pub fn messages_body(r: &Request) -> Value {
    let mut body = json!({
        "model": r.model,
        "max_tokens": r.max_tokens,
        "system": r.system,
        "messages": [{"role": "user", "content": r.user}],
    });
    if !r.tools.is_empty() {
        body["tools"] = r.tools.iter().map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.schema})).collect();
    }
    body
}

/// The body of `POST /chat/completions`.
pub fn chat_body(r: &Request) -> Value {
    let mut body = json!({
        "model": r.model,
        "messages": [
            {"role": "system", "content": r.system},
            {"role": "user", "content": r.user},
        ],
    });
    if !r.tools.is_empty() {
        body["tools"] = r
            .tools
            .iter()
            .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.schema}}))
            .collect();
    }
    body
}

fn parse(body: &str) -> Result<Value, AiError> {
    serde_json::from_str(body).map_err(|e| AiError::BadReply(format!("not JSON: {e}")))
}

/// Reads a Messages API reply.
pub fn parse_messages(body: &str) -> Result<Reply, AiError> {
    let v = parse(body)?;
    let stop = v["stop_reason"].as_str().unwrap_or("");
    if stop == "refusal" {
        return Err(AiError::Refused);
    }
    let blocks = v["content"].as_array().ok_or_else(|| AiError::BadReply("no content".into()))?;
    let mut reply = Reply::default();
    for b in blocks {
        match b["type"].as_str() {
            Some("text") => reply.text.push_str(b["text"].as_str().unwrap_or("")),
            Some("tool_use") => reply.calls.push(ToolCall { name: b["name"].as_str().unwrap_or("").to_owned(), input: b["input"].clone() }),
            _ => {}
        }
    }
    if stop == "max_tokens" && reply.calls.is_empty() {
        return Err(AiError::TooLong);
    }
    Ok(reply)
}

/// Reads a Chat Completions reply. Tool arguments come as a JSON string.
pub fn parse_chat(body: &str) -> Result<Reply, AiError> {
    let v = parse(body)?;
    let choice = v["choices"].get(0).ok_or_else(|| AiError::BadReply("no choices".into()))?;
    let msg = &choice["message"];
    let mut reply = Reply { text: msg["content"].as_str().unwrap_or("").to_owned(), calls: Vec::new() };
    for call in msg["tool_calls"].as_array().into_iter().flatten() {
        let f = &call["function"];
        let name = f["name"].as_str().unwrap_or("").to_owned();
        let args = f["arguments"].as_str().unwrap_or("{}");
        let input = serde_json::from_str(args).map_err(|_| AiError::BadReply(format!("the arguments of {name} are not JSON")))?;
        reply.calls.push(ToolCall { name, input });
    }
    if choice["finish_reason"] == "length" && reply.calls.is_empty() {
        return Err(AiError::TooLong);
    }
    Ok(reply)
}

/// The model names in a `GET /models` reply (both formats), sorted.
pub fn parse_models(body: &str) -> Result<Vec<String>, AiError> {
    let v = parse(body)?;
    let data = v["data"].as_array().ok_or_else(|| AiError::BadReply("no model list".into()))?;
    let mut ids: Vec<String> = data.iter().filter_map(|m| m["id"].as_str().map(str::to_owned)).collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// The error for an HTTP status other than 2xx. Both formats put the
/// reason in `error.message`.
pub fn error_for(status: u16, body: &str) -> AiError {
    match status {
        401 | 403 => AiError::BadKey,
        429 => AiError::RateLimited,
        _ => {
            let message = serde_json::from_str::<Value>(body)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
                .unwrap_or_else(|| body.chars().take(200).collect());
            AiError::Api { status, message }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fix_tool() -> Tool {
        Tool { name: "propose_command", description: "d", schema: serde_json::json!({"type":"object"}) }
    }
    fn req(tools: &[Tool]) -> Request<'_> {
        Request { model: "m", system: "s", user: "u", tools, max_tokens: 100 }
    }

    #[test]
    fn claude_body_has_no_sampling_or_forced_tool() {
        let t = [fix_tool()];
        let b = messages_body(&req(&t));
        assert_eq!(b["system"], "s");
        assert_eq!(b["messages"][0]["content"], "u");
        assert_eq!(b["tools"][0]["input_schema"]["type"], "object");
        for k in ["temperature", "top_p", "top_k", "tool_choice", "thinking"] {
            assert!(b.get(k).is_none(), "{k}");
        }
    }

    #[test]
    fn chat_body_wraps_tools_as_functions() {
        let t = [fix_tool()];
        let b = chat_body(&req(&t));
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["tools"][0]["type"], "function");
        assert_eq!(b["tools"][0]["function"]["parameters"]["type"], "object");
        // No tools: no "tools" key (some servers reject an empty list).
        assert!(chat_body(&req(&[])).get("tools").is_none());
        assert!(messages_body(&req(&[])).get("tools").is_none());
    }

    #[test]
    fn claude_tool_call_and_text() {
        let r = parse_messages(r#"{"content":[{"type":"text","text":"Hi"},{"type":"tool_use","id":"t","name":"propose_command","input":{"command":"ls"}}],"stop_reason":"tool_use"}"#).unwrap();
        assert_eq!(r.text, "Hi");
        assert_eq!(r.calls[0].input["command"], "ls");
    }

    #[test]
    fn claude_refusal_and_cut_off() {
        assert_eq!(parse_messages(r#"{"content":[],"stop_reason":"refusal"}"#).unwrap_err(), AiError::Refused);
        assert_eq!(parse_messages(r#"{"content":[{"type":"text","text":"x"}],"stop_reason":"max_tokens"}"#).unwrap_err(), AiError::TooLong);
        assert!(matches!(parse_messages("not json"), Err(AiError::BadReply(_))));
    }

    #[test]
    fn chat_arguments_are_a_json_string() {
        let ok = r#"{"choices":[{"message":{"content":null,"tool_calls":[{"function":{"name":"propose_command","arguments":"{\"command\":\"tar -xzf a.tgz\"}"}}]},"finish_reason":"tool_calls"}]}"#;
        assert_eq!(parse_chat(ok).unwrap().calls[0].input["command"], "tar -xzf a.tgz");
        let bad = ok.replace("{\\\"command", "{oops");
        assert!(matches!(parse_chat(&bad), Err(AiError::BadReply(_))));
        let text_only = r#"{"choices":[{"message":{"content":"It worked."},"finish_reason":"stop"}]}"#;
        assert_eq!(parse_chat(text_only).unwrap().text, "It worked.");
        let cut = r#"{"choices":[{"message":{"content":"It"},"finish_reason":"length"}]}"#;
        assert_eq!(parse_chat(cut).unwrap_err(), AiError::TooLong);
        assert!(matches!(parse_chat(r#"{"choices":[]}"#), Err(AiError::BadReply(_))));
    }

    #[test]
    fn errors_and_models() {
        assert_eq!(error_for(401, "{}"), AiError::BadKey);
        assert_eq!(error_for(429, ""), AiError::RateLimited);
        assert_eq!(
            error_for(400, r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad model"}}"#),
            AiError::Api { status: 400, message: "bad model".into() }
        );
        assert_eq!(error_for(500, "oops"), AiError::Api { status: 500, message: "oops".into() });
        assert_eq!(parse_models(r#"{"data":[{"id":"b"},{"id":"a"},{"id":"b"}]}"#).unwrap(), vec!["a", "b"]);
    }
}
