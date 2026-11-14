//! Claude Code as a provider: each request runs the user's own installed,
//! unmodified `claude` in print mode (`claude -p`), which signs in by itself
//! (a Claude Pro or Max plan, or its own key). fastcnsl never sees or keeps
//! its credentials. The request goes in on stdin as one prompt (the
//! instructions, the tools and the conversation so far); the answer comes
//! back as JSON matching a schema of tool calls, so a Fix's tool loop works
//! as with the APIs. On Windows a `claude` installed in WSL is used when
//! Windows has none.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::AiError;
use super::wire::{Reply, Request, Tool, ToolCall};

/// The models offered: Claude Code's aliases, which follow its newest models.
pub const MODELS: [&str; 3] = ["sonnet", "opus", "haiku"];
/// Replaces Claude Code's own system prompt (its coding-agent one); the
/// real instructions are at the top of the prompt.
const SYSTEM: &str = "You answer a terminal app's request. Follow the instructions at the top of the user's message and answer only through the JSON schema.";
/// How long one run may take.
const RUN_TIME: Duration = Duration::from_secs(300);

/// How to start `claude`.
#[derive(Debug, Clone, PartialEq)]
pub struct Launcher {
    /// The program's path (inside WSL when `in_wsl`).
    pub program: String,
    /// Run through `wsl.exe` (Windows, with Claude Code installed in WSL).
    pub in_wsl: bool,
}

impl Launcher {
    /// "claude" or "claude in WSL", for messages.
    pub fn describe(&self) -> String {
        if self.in_wsl { format!("{} in WSL", self.program) } else { self.program.clone() }
    }

    /// The command that runs `claude args...` in a folder of no project, so
    /// no project's CLAUDE.md or settings join in.
    fn command(&self, args: &[String]) -> Command {
        if self.in_wsl {
            // A login shell, so a Claude Code installed with npm finds node.
            let mut c = Command::new("wsl.exe");
            c.args(["--cd", "/tmp", "-e", "sh", "-lc", "exec \"$0\" \"$@\"", &self.program]).args(args);
            no_window(&mut c);
            c
        } else {
            let mut c = Command::new(&self.program);
            c.args(args).current_dir(std::env::temp_dir());
            no_window(&mut c);
            c
        }
    }
}

/// Finds Claude Code: `claude` on PATH or in `~/.local/bin`, else (Windows)
/// in the default WSL distribution. The first one found is kept.
pub fn find() -> Result<Launcher, AiError> {
    static FOUND: std::sync::Mutex<Option<Launcher>> = std::sync::Mutex::new(None);
    let mut found = FOUND.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(l) = found.as_ref() {
        return Ok(l.clone());
    }
    let launcher = find_here().map(|p| Launcher { program: p.display().to_string(), in_wsl: false }).or_else(find_in_wsl).ok_or(AiError::NoClaudeCode)?;
    log::info!("AI: Claude Code is {}", launcher.describe());
    *found = Some(launcher.clone());
    Ok(launcher)
}

/// `claude` on PATH, else in `~/.local/bin` (where its installer puts it).
fn find_here() -> Option<PathBuf> {
    let name = if cfg!(windows) { "claude.exe" } else { "claude" };
    let path = std::env::var_os("PATH").unwrap_or_default();
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    std::env::split_paths(&path)
        .chain(home.map(|h| h.join(".local").join("bin")))
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// `claude` in the default WSL distribution, by its login shell's PATH or
/// in `~/.local/bin`.
#[cfg(windows)]
fn find_in_wsl() -> Option<Launcher> {
    let script = r#"command -v claude || { test -x "$HOME/.local/bin/claude" && echo "$HOME/.local/bin/claude"; }"#;
    let mut c = Command::new("wsl.exe");
    c.args(["-e", "sh", "-lc", script]).stdin(Stdio::null()).stderr(Stdio::null());
    no_window(&mut c);
    let out = c.output().ok()?;
    let program = String::from_utf8_lossy(&out.stdout).lines().map(str::trim).rfind(|l| l.starts_with('/'))?.to_owned();
    Some(Launcher { program, in_wsl: true })
}

#[cfg(not(windows))]
fn find_in_wsl() -> Option<Launcher> {
    None
}

/// No console window flashing up for `claude` or `wsl.exe` (Windows).
fn no_window(c: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let _ = c;
}

/// The arguments of one run: print mode, JSON out, no built-in tools, no
/// MCP servers, nothing saved, and the answer's schema when there is one.
pub fn args(model: &str, schema: Option<&Value>) -> Vec<String> {
    let mut a: Vec<String> = ["-p", "--output-format", "json", "--tools", "", "--strict-mcp-config", "--no-session-persistence", "--system-prompt", SYSTEM]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    if !model.is_empty() {
        a.extend(["--model".to_owned(), model.to_owned()]);
    }
    if let Some(schema) = schema {
        a.extend(["--json-schema".to_owned(), schema.to_string()]);
    }
    a
}

/// The schema of an answer: a sentence, and calls of `tools` each with
/// its own input.
pub fn schema(tools: &[Tool]) -> Value {
    let calls: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "type": "object",
                "properties": {"name": {"type": "string", "enum": [t.name]}, "input": t.schema},
                "required": ["name", "input"],
                "additionalProperties": false,
            })
        })
        .collect();
    json!({
        "type": "object",
        "properties": {
            "text": {"type": "string", "description": "Your answer in words, when you call no tool"},
            "calls": {"type": "array", "description": "The tools to call now, in order", "items": {"anyOf": calls}},
        },
        "required": ["calls"],
        "additionalProperties": false,
    })
}

/// The request as one prompt: the instructions, the tools, then the
/// conversation (user text, the calls made, their results).
pub fn prompt(r: &Request) -> String {
    let mut p = format!("{}\n\n", r.system);
    if !r.tools.is_empty() {
        p.push_str("You can call these tools (put the calls in your answer's \"calls\"; the results come back in the next message):\n");
        for t in r.tools {
            p.push_str(&format!("- {}: {} Input: {}\n", t.name, t.description, t.schema));
        }
        p.push('\n');
    }
    p.push_str("The conversation so far:\n");
    let mut names = std::collections::HashMap::new();
    for m in r.messages {
        match m["role"].as_str() {
            Some("user") => p.push_str(&format!("\n=== From the terminal ===\n{}\n", m["content"].as_str().unwrap_or(""))),
            Some("assistant") => {
                p.push_str("\n=== Your answer ===\n");
                if let Some(text) = m["content"].as_str().filter(|t| !t.is_empty()) {
                    p.push_str(&format!("{text}\n"));
                }
                for c in m["calls"].as_array().into_iter().flatten() {
                    let name = c["name"].as_str().unwrap_or("");
                    names.insert(c["id"].as_str().unwrap_or("").to_owned(), name.to_owned());
                    p.push_str(&format!("{name} {}\n", c["input"]));
                }
            }
            Some("tool") => {
                let id = m["call_id"].as_str().unwrap_or("");
                let name = names.get(id).map_or("tool", String::as_str);
                p.push_str(&format!("\n=== Result of {name} ===\n{}\n", m["content"].as_str().unwrap_or("")));
            }
            _ => {}
        }
    }
    p.push_str("\nGive your next answer.\n");
    p
}

/// Reads the JSON Claude Code printed; `expects_calls` when a schema was
/// sent. `stderr` explains a run that printed nothing.
pub fn parse(stdout: &str, stderr: &str, expects_calls: bool) -> Result<Reply, AiError> {
    // The result is the last JSON line (warnings may come first).
    let Some(v) = stdout.lines().rev().find_map(|l| serde_json::from_str::<Value>(l.trim()).ok().filter(|v| v.is_object())) else {
        let why = Some(stderr.trim()).filter(|e| !e.is_empty()).or(Some(stdout.trim()).filter(|o| !o.is_empty())).unwrap_or("it printed nothing");
        return Err(AiError::BadReply(format!("Claude Code: {}", why.chars().take(300).collect::<String>())));
    };
    let result = v["result"].as_str().unwrap_or("").to_owned();
    if v["is_error"].as_bool().unwrap_or(false) || v["subtype"].as_str().is_some_and(|s| s != "success") {
        let lower = result.to_lowercase();
        let status = v["api_error_status"].as_u64().unwrap_or(0) as u16;
        return Err(if lower.contains("not logged in") || lower.contains("/login") || status == 401 {
            AiError::ClaudeCodeSignIn
        } else if status == 429 || lower.contains("limit reached") {
            AiError::RateLimited
        } else {
            AiError::Api { status, message: result }
        });
    }
    if !expects_calls {
        return Ok(Reply { text: result, calls: Vec::new(), raw: Value::Null });
    }
    let answer = &v["structured_output"];
    if !answer.is_object() {
        return Err(AiError::BadReply(format!("Claude Code gave no answer in the schema{}", if result.is_empty() { String::new() } else { format!(": {}", result.chars().take(200).collect::<String>()) })));
    }
    let calls: Vec<ToolCall> = answer["calls"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, c)| ToolCall { id: format!("call{}", i + 1), name: c["name"].as_str().unwrap_or("").to_owned(), input: c["input"].clone() })
        .collect();
    Ok(Reply { text: answer["text"].as_str().unwrap_or("").to_owned(), calls, raw: Value::Null })
}

/// One request through Claude Code.
pub fn send(launcher: &Launcher, r: &Request) -> Result<Reply, AiError> {
    let schema = (!r.tools.is_empty()).then(|| schema(r.tools));
    let started = Instant::now();
    let mut child = launcher
        .command(&args(r.model, schema.as_ref()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AiError::BadReply(format!("could not start Claude Code ({}): {e}", launcher.describe())))?;
    // Read both pipes while writing the prompt, so neither fills up.
    let reader = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_string(&mut text);
            }
            text
        })
    };
    let out = reader(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = reader(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt(r).as_bytes());
    }
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > RUN_TIME => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AiError::Network("Claude Code took too long".into()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(AiError::BadReply(format!("Claude Code: {e}"))),
        }
    }
    let (stdout, stderr) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    // Never the prompt or the answer: they hold terminal text.
    log::info!("AI: claude-code run: {} ms", started.elapsed().as_millis());
    parse(&stdout, &stderr, schema.is_some())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::path::Path;

    use super::*;
    use crate::ai::wire::{Format, assistant_turn, tool_results, user_message};

    fn tools() -> [Tool; 3] {
        crate::ai::fix::tools()
    }

    #[test]
    fn a_run_has_no_tools_no_servers_saves_nothing_and_names_model_and_schema() {
        let schema = json!({"type": "object"});
        let a = args("opus", Some(&schema));
        let pairs: Vec<(&str, &str)> = a.windows(2).map(|w| (w[0].as_str(), w[1].as_str())).collect();
        assert_eq!(a[0], "-p");
        for pair in [("--output-format", "json"), ("--tools", ""), ("--model", "opus"), ("--json-schema", r#"{"type":"object"}"#), ("--system-prompt", SYSTEM)] {
            assert!(pairs.contains(&pair), "{pair:?} in {a:?}");
        }
        assert!(a.iter().any(|x| x == "--strict-mcp-config") && a.iter().any(|x| x == "--no-session-persistence"));
        let plain = args("", None);
        assert!(!plain.iter().any(|x| x == "--model" || x == "--json-schema"), "{plain:?}");
    }

    #[test]
    fn the_schema_gives_each_tool_its_own_input() {
        let s = schema(&tools());
        assert_eq!(s["required"], json!(["calls"]));
        let options = s["properties"]["calls"]["items"]["anyOf"].as_array().unwrap();
        assert_eq!(options.len(), 3);
        let propose = options.iter().find(|o| o["properties"]["name"]["enum"] == json!(["propose_command"])).unwrap();
        assert_eq!(propose["properties"]["input"]["required"], json!(["command", "explanation"]));
        assert_eq!(s["properties"]["text"]["type"], "string");
    }

    #[test]
    fn the_prompt_holds_instructions_tools_and_the_conversation() {
        let reply = Reply {
            text: String::new(),
            calls: vec![ToolCall { id: "call1".into(), name: "get_output".into(), input: json!({"line": 12}) }],
            raw: Value::Null,
        };
        let mut messages = vec![user_message("FIRST MESSAGE")];
        messages.extend(assistant_turn(Format::ClaudeCode, &reply));
        messages.extend(tool_results(Format::ClaudeCode, &[("call1".into(), "OUTPUT OF 12".into())]));
        let t = tools();
        let p = prompt(&Request { model: "m", system: "THE INSTRUCTIONS", messages: &messages, tools: &t, max_tokens: 1 });
        let at = |s: &str| p.find(s).unwrap_or_else(|| panic!("{s} in {p}"));
        assert!(at("THE INSTRUCTIONS") < at("get_output:"));
        assert!(at("propose_command:") < at("FIRST MESSAGE"));
        assert!(at("FIRST MESSAGE") < at(r#"get_output {"line":12}"#));
        assert!(at(r#"get_output {"line":12}"#) < at("OUTPUT OF 12"));
    }

    #[test]
    fn structured_calls_become_tool_calls() {
        let out = r#"{"type":"result","subtype":"success","is_error":false,"result":"","structured_output":{"text":"","calls":[{"name":"add_bookmark","input":{"line":3}},{"name":"propose_command","input":{"command":"ls -l","explanation":"typo"}}]}}"#;
        let r = parse(out, "", true).unwrap();
        assert_eq!(r.calls.len(), 2);
        assert_eq!((r.calls[1].name.as_str(), r.calls[1].input["command"].as_str()), ("propose_command", Some("ls -l")));
        assert_ne!(r.calls[0].id, r.calls[1].id, "each call has its own id");
    }

    #[test]
    fn without_a_schema_the_answer_is_the_text() {
        let r = parse(r#"{"type":"result","subtype":"success","is_error":false,"result":"ok"}"#, "", false).unwrap();
        assert_eq!((r.text.as_str(), r.calls.len()), ("ok", 0));
        let none = parse(r#"{"type":"result","subtype":"success","is_error":false,"result":"hmm"}"#, "", true);
        assert!(matches!(none, Err(AiError::BadReply(_))), "{none:?}");
    }

    #[test]
    fn failures_are_explained() {
        let signed_out = r#"{"type":"result","subtype":"success","is_error":true,"result":"Not logged in · Please run /login"}"#;
        assert_eq!(parse(signed_out, "", true).unwrap_err(), AiError::ClaudeCodeSignIn);
        let limit = r#"{"type":"result","subtype":"success","is_error":true,"api_error_status":429,"result":"Claude AI usage limit reached"}"#;
        assert_eq!(parse(limit, "", true).unwrap_err(), AiError::RateLimited);
        let other = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"api_error_status":500,"result":"Overloaded"}"#;
        assert_eq!(parse(other, "", true).unwrap_err(), AiError::Api { status: 500, message: "Overloaded".into() });
        assert_eq!(parse("", "error: unknown option '--json-schema'", true).unwrap_err(), AiError::BadReply("Claude Code: error: unknown option '--json-schema'".into()));
    }

    /// A fake `claude` that keeps its arguments and prompt and prints what
    /// `answers` holds, one answer per run.
    #[cfg(unix)]
    fn fake_claude(dir: &Path, answers: &[&str]) -> Launcher {
        use std::os::unix::fs::PermissionsExt;
        for (i, a) in answers.iter().enumerate() {
            std::fs::write(dir.join(format!("answer{i}")), a).unwrap();
        }
        let script = format!(
            "#!/bin/sh\nd='{d}'\nn=$(cat \"$d/count\" 2>/dev/null || echo 0)\nprintf '%s\\n' \"$@\" > \"$d/args$n\"\ncat > \"$d/prompt$n\"\npwd > \"$d/cwd$n\"\ncat \"$d/answer$n\"\necho $((n+1)) > \"$d/count\"\n",
            d = dir.display()
        );
        let path = dir.join("claude");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        Launcher { program: path.display().to_string(), in_wsl: false }
    }

    #[cfg(unix)]
    #[test]
    fn a_fix_runs_through_claude_code_with_its_tool_loop() {
        let dir = crate::testterm::temp_dir("claude-code-fix");
        let launcher = fake_claude(
            &dir,
            &[
                r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"calls":[{"name":"get_output","input":{"line":7}}]}}"#,
                r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"calls":[{"name":"propose_command","input":{"command":"tar -xzf a.tgz","explanation":"z for gzip"}}]}}"#,
            ],
        );
        let client = crate::ai::client::Client::claude_code_with(launcher);
        let mut asked = Vec::new();
        let fix = crate::ai::fix::run(&client, "sonnet", "THE CONTEXT".into(), |calls| {
            asked.extend(calls.iter().map(|c| c.name.clone()));
            Some(calls.iter().map(|_| "OUTPUT SEVEN".to_owned()).collect())
        })
        .unwrap();
        assert_eq!(fix.command.as_deref(), Some("tar -xzf a.tgz"));
        assert_eq!(asked, ["get_output"]);
        let second = std::fs::read_to_string(dir.join("prompt1")).unwrap();
        assert!(second.contains("THE CONTEXT") && second.contains("OUTPUT SEVEN"), "{second}");
        let args = std::fs::read_to_string(dir.join("args0")).unwrap();
        assert!(args.lines().any(|l| l == "--json-schema") && args.lines().any(|l| l == "sonnet"), "{args}");
        let cwd = std::fs::read_to_string(dir.join("cwd0")).unwrap();
        assert_eq!(Path::new(cwd.trim()), std::env::temp_dir().canonicalize().unwrap(), "runs in no project");
    }

    /// Uses the real Claude Code and the user's plan:
    /// `cargo test real_claude_code -- --ignored`.
    #[test]
    #[ignore]
    fn real_claude_code_proposes_a_command() {
        let client = crate::ai::client::Client::claude_code_with(find().unwrap());
        let first = "Recent commands:\n1: $ tar -xvf archive.tar.gz -C   <- fix this one\nOutput of line 1:\ntar: option requires an argument -- 'C'";
        let fix = crate::ai::fix::run(&client, "haiku", first.into(), |calls| Some(calls.iter().map(|_| "no output".to_owned()).collect())).unwrap();
        assert!(fix.command.is_some(), "{fix:?}");
    }
}
