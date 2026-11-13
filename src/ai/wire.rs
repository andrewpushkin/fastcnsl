//! The JSON of requests and replies, for Anthropic's Messages API and the
//! Chat Completions format (OpenAI, xAI, and servers that copy it). Pure
//! functions: the client in `client.rs` does the HTTP.

use serde_json::{Value, json};

use super::AiError;

/// The API format a provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Anthropic's Messages API.
    Messages,
    /// Chat Completions (OpenAI with a key, xAI, local servers).
    Chat,
    /// OpenAI's Responses API, the only one a ChatGPT plan can use.
    Responses,
}

/// A tool the model may call.
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON Schema of its input.
    pub schema: Value,
}

/// One request: a system prompt, the conversation so far and the tools.
pub struct Request<'a> {
    pub model: &'a str,
    pub system: &'a str,
    /// User and assistant turns and tool results, oldest first, in the
    /// provider's format (see [`user_message`], [`assistant_message`],
    /// [`tool_results`]).
    pub messages: &'a [Value],
    pub tools: &'a [Tool],
    /// Limit on the reply (Messages API only: Chat Completions servers
    /// differ in what they call it, and their defaults are fine).
    pub max_tokens: u32,
}

/// A call the model made to one of the tools.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// The provider's id for the call, which its result must carry.
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// What the model answered.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Reply {
    pub text: String,
    pub calls: Vec<ToolCall>,
    /// The answer as received (Claude: its `content`; Chat Completions:
    /// the message), sent back unchanged in the next request.
    pub raw: Value,
}

/// A user turn (the same in both formats).
pub fn user_message(text: &str) -> Value {
    json!({"role": "user", "content": text})
}

/// The model's answer as the next request repeats it. Claude's content goes
/// back exactly as received: it can hold thinking blocks that must not change.
/// For Responses: its text, then its function calls (`raw`, already cut down
/// to what may be sent back with `store: false`).
pub fn assistant_turn(f: Format, reply: &Reply) -> Vec<Value> {
    match f {
        Format::Messages => vec![json!({"role": "assistant", "content": reply.raw})],
        Format::Chat => vec![reply.raw.clone()],
        Format::Responses => {
            let text = (!reply.text.is_empty()).then(|| json!({"role": "assistant", "content": reply.text}));
            text.into_iter().chain(reply.raw.as_array().into_iter().flatten().cloned()).collect()
        }
    }
}

/// The results of the model's tool calls, as `(call id, text)` pairs.
/// Claude takes them in one user turn; the others one item each.
pub fn tool_results(f: Format, results: &[(String, String)]) -> Vec<Value> {
    match f {
        Format::Messages => {
            let blocks: Vec<Value> = results.iter().map(|(id, text)| json!({"type": "tool_result", "tool_use_id": id, "content": text})).collect();
            vec![json!({"role": "user", "content": blocks})]
        }
        Format::Chat => results.iter().map(|(id, text)| json!({"role": "tool", "tool_call_id": id, "content": text})).collect(),
        Format::Responses => results.iter().map(|(id, text)| json!({"type": "function_call_output", "call_id": id, "output": text})).collect(),
    }
}

/// The body of `POST /v1/messages`. No sampling settings, no thinking
/// settings and no forced tool: current Claude models reject them.
pub fn messages_body(r: &Request) -> Value {
    let mut body = json!({
        "model": r.model,
        "max_tokens": r.max_tokens,
        "system": r.system,
        "messages": r.messages,
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
        "messages": std::iter::once(json!({"role": "system", "content": r.system})).chain(r.messages.iter().cloned()).collect::<Vec<_>>(),
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

/// The body of `POST /responses` for a ChatGPT plan: nothing stored,
/// always streamed, and no token-limit or sampling fields (the plan
/// rejects them).
pub fn responses_body(r: &Request) -> Value {
    let mut body = json!({
        "model": r.model,
        "instructions": r.system,
        "input": r.messages,
        "store": false,
        "stream": true,
    });
    if !r.tools.is_empty() {
        body["tools"] = r.tools.iter().map(|t| json!({"type": "function", "name": t.name, "description": t.description, "parameters": t.schema})).collect();
    }
    body
}

/// Reads a streamed Responses reply (server-sent events) to its end. Only
/// the final `response.completed` event is used: it holds the whole answer.
pub fn parse_responses_stream(body: impl std::io::BufRead) -> Result<Reply, AiError> {
    for line in body.lines() {
        let line = line.map_err(|e| AiError::Network(e.to_string()))?;
        let Some(data) = line.strip_prefix("data:") else { continue };
        let Ok(event) = serde_json::from_str::<Value>(data.trim()) else { continue };
        match event["type"].as_str() {
            Some("response.completed") => return Ok(responses_reply(&event["response"]["output"])),
            Some("response.failed") => {
                let e = &event["response"]["error"];
                return Err(error_for_code(e["code"].as_str().unwrap_or(""), e["message"].as_str().unwrap_or("the request failed")));
            }
            Some("response.incomplete") => return Err(AiError::TooLong),
            Some("error") => {
                return Err(error_for_code(event["code"].as_str().unwrap_or(""), event["message"].as_str().unwrap_or("the request failed")));
            }
            _ => {}
        }
    }
    Err(AiError::BadReply("the reply stopped".into()))
}

/// A Reply from a Responses `output` list. Reasoning items are dropped and
/// function calls keep only what can be sent back without stored ids.
fn responses_reply(output: &Value) -> Reply {
    let mut reply = Reply::default();
    let mut raw = Vec::new();
    for item in output.as_array().into_iter().flatten() {
        match item["type"].as_str() {
            Some("message") => {
                for part in item["content"].as_array().into_iter().flatten() {
                    if part["type"] == "output_text" {
                        reply.text.push_str(part["text"].as_str().unwrap_or(""));
                    }
                }
            }
            Some("function_call") => {
                let args = item["arguments"].as_str().unwrap_or("{}");
                let call_id = item["call_id"].as_str().unwrap_or("").to_owned();
                let name = item["name"].as_str().unwrap_or("").to_owned();
                raw.push(json!({"type": "function_call", "call_id": call_id, "name": name, "arguments": args}));
                let input = serde_json::from_str(args).unwrap_or(Value::Null);
                reply.calls.push(ToolCall { id: call_id, name, input });
            }
            _ => {}
        }
    }
    reply.raw = Value::Array(raw);
    reply
}

/// The error for a ChatGPT plan's error code (others: an API error).
pub fn error_for_code(code: &str, message: &str) -> AiError {
    match code {
        "subscription_sharing_user_not_eligible" => AiError::PlanNotEligible,
        "subscription_sharing_usage_limit_exceeded" => AiError::PlanLimit,
        "subscription_sharing_usage_unavailable" | "subscription_sharing_user_unavailable" => AiError::PlanBusy,
        "subscription_sharing_invalid_user" => AiError::SignInAgain,
        _ => AiError::Api { status: 0, message: if code.is_empty() { message.to_owned() } else { format!("{code}: {message}") } },
    }
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
    let mut reply = Reply { raw: v["content"].clone(), ..Reply::default() };
    for b in blocks {
        match b["type"].as_str() {
            Some("text") => reply.text.push_str(b["text"].as_str().unwrap_or("")),
            Some("tool_use") => reply.calls.push(ToolCall {
                id: b["id"].as_str().unwrap_or("").to_owned(),
                name: b["name"].as_str().unwrap_or("").to_owned(),
                input: b["input"].clone(),
            }),
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
    let mut reply = Reply { text: msg["content"].as_str().unwrap_or("").to_owned(), calls: Vec::new(), raw: msg.clone() };
    for call in msg["tool_calls"].as_array().into_iter().flatten() {
        let id = call["id"].as_str().unwrap_or("").to_owned();
        let f = &call["function"];
        let name = f["name"].as_str().unwrap_or("").to_owned();
        let args = f["arguments"].as_str().unwrap_or("{}");
        let input = serde_json::from_str(args).map_err(|_| AiError::BadReply(format!("the arguments of {name} are not JSON")))?;
        reply.calls.push(ToolCall { id, name, input });
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
    // A ChatGPT plan's list marks the models to offer with visibility "list".
    let offered = |m: &&Value| m.get("visibility").is_none_or(|v| v == "list");
    let mut ids: Vec<String> = data.iter().filter(offered).filter_map(|m| m["id"].as_str().map(str::to_owned)).collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// The error for an HTTP status other than 2xx. Both formats put the
/// reason in `error.message`.
pub fn error_for(status: u16, body: &str) -> AiError {
    let v = serde_json::from_str::<Value>(body).ok();
    if let Some(code) = v.as_ref().and_then(|v| v["error"]["code"].as_str())
        && code.starts_with("subscription_sharing_")
    {
        return error_for_code(code, "");
    }
    if let Some(detail) = v.as_ref().and_then(|v| v["detail"].as_str()) {
        return AiError::Api { status, message: detail.to_owned() };
    }
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
    fn req<'a>(tools: &'a [Tool], messages: &'a [Value]) -> Request<'a> {
        Request { model: "m", system: "s", messages, tools, max_tokens: 100 }
    }

    #[test]
    fn claude_body_has_no_sampling_or_forced_tool() {
        let t = [fix_tool()];
        let u = [user_message("u")];
        let b = messages_body(&req(&t, &u));
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
        let u = [user_message("u")];
        let b = chat_body(&req(&t, &u));
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["tools"][0]["type"], "function");
        assert_eq!(b["tools"][0]["function"]["parameters"]["type"], "object");
        // No tools: no "tools" key (some servers reject an empty list).
        assert!(chat_body(&req(&[], &u)).get("tools").is_none());
        assert!(messages_body(&req(&[], &u)).get("tools").is_none());
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
    fn claude_assistant_turn_is_echoed_verbatim() {
        let body = r#"{"content":[{"type":"thinking","thinking":"","signature":"sig"},{"type":"tool_use","id":"tu1","name":"get_output","input":{"line":3}}],"stop_reason":"tool_use"}"#;
        let r = parse_messages(body).unwrap();
        assert_eq!(r.calls[0].id, "tu1");
        let a = &assistant_turn(Format::Messages, &r)[0];
        assert_eq!(a["role"], "assistant");
        assert_eq!(a["content"][0]["signature"], "sig");
        let t = tool_results(Format::Messages, &[("tu1".into(), "out".into())]);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0]["content"][0]["tool_use_id"], "tu1");
    }

    #[test]
    fn chat_tool_results_are_one_message_each() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"c1","type":"function","function":{"name":"get_output","arguments":"{\"line\":3}"}}]},"finish_reason":"tool_calls"}]}"#;
        let r = parse_chat(body).unwrap();
        assert_eq!(r.calls[0].id, "c1");
        assert_eq!(assistant_turn(Format::Chat, &r)[0]["tool_calls"][0]["id"], "c1");
        let t = tool_results(Format::Chat, &[("c1".into(), "a".into()), ("c2".into(), "b".into())]);
        assert_eq!((t.len(), t[1]["role"].as_str(), t[1]["tool_call_id"].as_str()), (2, Some("tool"), Some("c2")));
    }

    #[test]
    fn bodies_carry_the_whole_conversation() {
        let msgs = [user_message("u"), serde_json::json!({"role":"assistant","content":"a"}), user_message("v")];
        let r = Request { model: "m", system: "s", messages: &msgs, tools: &[], max_tokens: 10 };
        assert_eq!(messages_body(&r)["messages"].as_array().unwrap().len(), 3);
        let chat = chat_body(&r);
        assert_eq!(chat["messages"].as_array().unwrap().len(), 4);
        assert_eq!(chat["messages"][0]["role"], "system");
    }

    #[test]
    fn responses_body_follows_the_plan_rules() {
        let msgs = [user_message("u")];
        let tools = [Tool { name: "t", description: "d", schema: serde_json::json!({"type":"object"}) }];
        let b = responses_body(&Request { model: "m", system: "s", messages: &msgs, tools: &tools, max_tokens: 10 });
        assert_eq!((b["instructions"].as_str(), b["store"].as_bool(), b["stream"].as_bool()), (Some("s"), Some(false), Some(true)));
        assert_eq!(b["input"][0]["content"], "u");
        assert_eq!(b["tools"][0]["type"], "function");
        assert_eq!(b["tools"][0]["name"], "t");
        for k in ["max_output_tokens", "temperature", "top_p", "previous_response_id", "max_tokens"] {
            assert!(b.get(k).is_none(), "{k}");
        }
    }

    #[test]
    fn a_stream_with_a_function_call() {
        let sse = "event: response.created\ndata: {\"type\":\"response.created\"}\n\n\
                   data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\n\n\
                   data: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"type\":\"reasoning\",\"id\":\"rs_1\"},{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hi\"}]},{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"get_output\",\"arguments\":\"{\\\"line\\\":3}\"}]}}\n\n";
        let r = parse_responses_stream(sse.as_bytes()).unwrap();
        assert_eq!(r.text, "Hi");
        assert_eq!((r.calls[0].id.as_str(), r.calls[0].input["line"].as_i64()), ("call_1", Some(3)));
        let turn = assistant_turn(Format::Responses, &r);
        assert_eq!(turn[0]["role"], "assistant");
        assert_eq!(turn[1]["call_id"], "call_1");
        assert!(turn[1].get("id").is_none() && turn.iter().all(|i| i["type"] != "reasoning"));
        let out = tool_results(Format::Responses, &[("call_1".into(), "x".into())]);
        assert_eq!((out[0]["type"].as_str(), out[0]["output"].as_str()), (Some("function_call_output"), Some("x")));
    }

    #[test]
    fn stream_failures_map_to_plan_errors() {
        let failed = "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"subscription_sharing_usage_limit_exceeded\",\"message\":\"m\"}}}\n\n";
        assert_eq!(parse_responses_stream(failed.as_bytes()).unwrap_err(), AiError::PlanLimit);
        assert_eq!(parse_responses_stream("data: {\"type\":\"response.incomplete\"}\n\n".as_bytes()).unwrap_err(), AiError::TooLong);
        assert!(matches!(parse_responses_stream("data: {\"type\":\"response.created\"}\n\n".as_bytes()), Err(AiError::BadReply(_))));
        assert_eq!(error_for(403, r#"{"error":{"code":"subscription_sharing_user_not_eligible","message":"no"}}"#), AiError::PlanNotEligible);
        assert_eq!(error_for(401, r#"{"error":{"code":"subscription_sharing_invalid_user"}}"#), AiError::SignInAgain);
        assert_eq!(error_for(503, r#"{"detail":"busy"}"#), AiError::Api { status: 503, message: "busy".into() });
        assert_eq!(parse_models(r#"{"data":[{"id":"a","visibility":"list"},{"id":"b","visibility":"hidden"},{"id":"c"}]}"#).unwrap(), vec!["a", "c"]);
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
