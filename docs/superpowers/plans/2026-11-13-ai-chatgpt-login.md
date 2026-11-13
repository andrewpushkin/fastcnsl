# AI stage 1c: ChatGPT sign-in — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** With provider ChatGPT, Options → AI offers **Continue with ChatGPT**; once signed in, Fix runs on the user's ChatGPT plan through the Responses API instead of an API key.

**Architecture:** `wire.rs` gets a `Format` enum (Messages, Chat, Responses) and the Responses body/stream parser. A new `src/ai/login.rs` holds the pure OAuth pieces (PKCE, authorize URL, callback and token parsing, the saved record) and the network steps (sign-in with a loopback listener, token exchange, refresh, revoke). `Client` picks the Responses format and refreshes tokens when a ChatGPT session exists. The app adds the Account row, the welcome panel and `AiEvent::SignedIn`.

**Tech Stack:** `ureq`, `serde_json`, plus direct deps on crates already in the tree: `ring = "0.17.14"` (SHA-256, secure random), `base64 = "0.22.1"` (URL-safe, no padding).

**Spec:** `docs/superpowers/specs/2026-11-13-ai-assistant-design.md`, "ChatGPT sign-in (stage 1c)".

## Global Constraints

- Auth base `https://auth.openai.com`, API base `https://api.openai.com/v1`; debug builds only: `FASTCNSL_CHATGPT_AUTH` / `FASTCNSL_CHATGPT_API` override them.
- Redirect `http://127.0.0.1:{port}/auth/callback`, port 1455 or any free one; never `localhost`.
- Scope `openid profile email offline_access resource.invoke chatgpt.tokens.use.direct`; `resource=https://api.openai.com/v1` on authorize, exchange and refresh.
- First sign-in `client_id=dynamic_agent_client` + `agent_name_hint=fastcnsl`; token exchange and refresh always use the issued client id.
- Responses requests: `store: false`, `stream: true`, `instructions`; never `max_output_tokens`, `temperature`, `top_p`, `previous_response_id`, or a `system` role item.
- Tokens never in URLs or logs; `chatgpt-login.json` written atomically, 0600 on Unix.
- Plain-text button label exactly `Continue with ChatGPT`; status `Using ChatGPT plan`; **Manage usage** → `https://chatgpt.com/settings/usage`; welcome panel title `You're using your ChatGPT plan`, button `Got it`, shown once.
- `cargo test` and `cargo clippy --all-targets -- -D warnings` clean per task; `make check-all` (Windows) at the end.

## Review Focus

1. The browser comes back with a wrong `state`, `error=access_denied`, or no `code` → sign-in fails with a message, nothing saved (Task 2 `callbacks_are_checked`).
2. The plan isn't eligible (scope lacks `chatgpt.tokens.use.direct`) → "this ChatGPT plan can't be used by other apps", tokens not saved (Task 2 `tokens_without_plan_scope_are_refused`).
3. The access token expired while the app was idle → the next Fix refreshes first and goes through (Task 3 `an_expired_token_is_refreshed_first`).
4. The refresh token was revoked in ChatGPT's settings → tokens cleared, "Sign in to ChatGPT again" (Task 3 `a_dead_refresh_token_signs_out`).
5. Usage limit hit mid-stream (`response.failed` with `subscription_sharing_usage_limit_exceeded`) → the limit message, not "unexpected reply" (Task 1 `stream_failures_map_to_plan_errors`).

---

### Task 1: The Responses format

**Files:** `src/ai/wire.rs`, `src/ai/mod.rs`, `src/ai/client.rs`, `src/ai/fix.rs`

**Interfaces:**
- `#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub enum Format { Messages, Chat, Responses }`; `assistant_message(messages_api: bool, …)` becomes `pub fn assistant_turn(f: Format, reply: &Reply) -> Vec<Value>`; `tool_results(f: Format, …)`; `Client::messages_api()` becomes `Client::format() -> Format`.
- `pub fn responses_body(r: &Request) -> Value` — `{model, instructions: system, input: messages, store: false, stream: true, tools: [{type:"function", name, description, parameters}]}` (no `tools` key when empty).
- `pub fn parse_responses_stream(body: impl BufRead) -> Result<Reply, AiError>` — reads SSE `data:` lines; on `response.completed` builds the Reply from `response.output`: `message` items' `output_text` parts → text; `function_call` items → `ToolCall { id: call_id, name, input: parse(arguments) }`; `raw` = array of the `function_call` items with only `type, call_id, name, arguments`. `response.failed` → `error_for_code(code, message)`; `response.incomplete` → `TooLong`; stream end without terminal event → `BadReply("the reply stopped")`.
- Responses turns: user = `{"role":"user","content":text}`; `assistant_turn` = `reply.raw` items, plus `{"role":"assistant","content":text}` first when text is non-empty; `tool_results` = `{"type":"function_call_output","call_id":id,"output":text}` each.
- `AiError` gains `PlanNotEligible`, `PlanLimit`, `PlanBusy`, `SignInAgain` with texts from the spec's Errors paragraph ("Manage usage" mentioned for `PlanLimit`).
- `pub fn error_for_code(code: &str, message: &str) -> AiError` and `error_for` checks `error.code` (and a `{"detail": …}` body) before the status rules.
- `parse_models` keeps entries whose `visibility` is absent or `"list"`.

- [ ] **Step 1: Failing tests** (`wire.rs`)

```rust
#[test]
fn responses_body_follows_the_plan_rules() {
    let msgs = [user_message("u")];
    let tools = [Tool { name: "t", description: "d", schema: serde_json::json!({"type":"object"}) }];
    let b = responses_body(&Request { model: "m", system: "s", messages: &msgs, tools: &tools, max_tokens: 10 });
    assert_eq!((b["instructions"].as_str(), b["store"].as_bool(), b["stream"].as_bool()), (Some("s"), Some(false), Some(true)));
    assert_eq!(b["tools"][0]["type"], "function");
    assert_eq!(b["tools"][0]["name"], "t");
    for k in ["max_output_tokens", "temperature", "top_p", "previous_response_id", "max_tokens"] { assert!(b.get(k).is_none(), "{k}"); }
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
```

- [ ] **Step 2: Run** `cargo test ai::wire` → FAIL. **Step 3: Implement** (Messages/Chat behaviour unchanged; update callers and tests to `Format`). **Step 4:** `cargo test`, clippy → PASS. **Step 5: Commit** `AI: the Responses format, for ChatGPT plans`

---

### Task 2: Sign-in pieces (pure) and the saved login

**Files:** Create `src/ai/login.rs`; `Cargo.toml` (`ring`, `base64`); `src/ai/mod.rs`

**Interfaces:**
- `pub const AUTH: &str = "https://auth.openai.com"; pub const API: &str = "https://api.openai.com/v1"; pub fn auth_base() -> String; pub fn api_base() -> String` (env overrides only when `cfg!(debug_assertions)`).
- `#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)] pub struct Login { client_id: String, host_id: String, email: String, id_token: String, access_token: String, refresh_token: String, expires_at: u64 /* unix s */, scopes: Vec<String>, welcomed: bool }` (all `pub`, `#[serde(default)]`).
- `impl Login { pub fn path() -> Option<PathBuf> /* beside config: chatgpt-login.json */; pub fn load(path) -> Login /* missing/broken → default */; pub fn save(&self, path) -> io::Result<()> /* write tmp + rename, 0600 */; pub fn signed_in(&self) -> bool /* refresh_token non-empty and scope ok */; pub fn needs_refresh(&self, now: u64) -> bool /* now + 60 >= expires_at */; pub fn sign_out(&mut self) /* clears email, tokens, expiry, scopes; keeps client_id, host_id, welcomed */ }`
- `pub fn random_token(bytes: usize) -> String` (ring SystemRandom, base64url no pad); `pub fn challenge(verifier: &str) -> String` (base64url SHA-256).
- `pub struct Pending { pub state: String, pub nonce: String, pub verifier: String, pub redirect: String }`
- `pub fn authorize_url(auth: &str, login: &Login, p: &Pending) -> String` — spec step 2 parameters, percent-encoded.
- `pub fn parse_callback(target: &str, p: &Pending, saved_client: &str) -> Result<(String /*code*/, String /*client_id*/), String>` — `target` is the request path+query.
- `pub fn parse_tokens(body: &str, now: u64) -> Result<Tokens, AiError>` with `pub struct Tokens { access_token, refresh_token, id_token, expires_at, scopes: Vec<String> }`; scope missing `chatgpt.tokens.use.direct` → `AiError::PlanNotEligible`.
- `pub fn id_token_claims(id_token: &str) -> Option<(String /*email*/, String /*nonce*/)>` — base64url-decodes the payload; no signature check.

- [ ] **Step 1: Failing tests** (`login.rs`)

```rust
fn pending() -> Pending { Pending { state: "st".into(), nonce: "no".into(), verifier: "v".repeat(43), redirect: "http://127.0.0.1:1455/auth/callback".into() } }

#[test]
fn pkce_challenge_is_base64url_sha256() {
    // RFC 7636 appendix B
    assert_eq!(challenge("dBjftJeZ4CVP-mJ92K9pTAgf-ZZqJNx4Xx1pTRxv8GM"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    let t = random_token(32);
    assert_eq!(t.len(), 43);
    assert_ne!(t, random_token(32));
}

#[test]
fn first_authorize_url_registers_fastcnsl() {
    let login = Login { host_id: "urn:uuid:1".into(), ..Login::default() };
    let u = authorize_url(AUTH, &login, &pending());
    assert!(u.starts_with("https://auth.openai.com/api/accounts/authorize?"));
    for part in ["client_id=dynamic_agent_client", "agent_name_hint=fastcnsl", "ext_agent_host_id=urn%3Auuid%3A1", "response_type=code",
                 "redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback", "scope=openid%20profile%20email%20offline_access%20resource.invoke%20chatgpt.tokens.use.direct",
                 "resource=https%3A%2F%2Fapi.openai.com%2Fv1", "state=st", "nonce=no", "code_challenge_method=S256"] {
        assert!(u.contains(part), "{part} in {u}");
    }
    let again = authorize_url(AUTH, &Login { client_id: "oaiapp_9".into(), id_token: "idt".into(), ..login }, &pending());
    assert!(again.contains("client_id=oaiapp_9") && again.contains("id_token_hint=idt") && !again.contains("agent_name_hint"));
}

#[test]
fn callbacks_are_checked() {
    let p = pending();
    assert_eq!(parse_callback("/auth/callback?code=c1&state=st&client_id=oaiapp_9", &p, ""), Ok(("c1".into(), "oaiapp_9".into())));
    assert_eq!(parse_callback("/auth/callback?code=c1&state=st", &p, "oaiapp_9"), Ok(("c1".into(), "oaiapp_9".into())));
    assert!(parse_callback("/auth/callback?code=c1&state=evil&client_id=x", &p, "").is_err());
    assert!(parse_callback("/auth/callback?error=access_denied&state=st", &p, "").is_err());
    assert!(parse_callback("/auth/callback?state=st&client_id=x", &p, "").is_err());
    assert!(parse_callback("/auth/callback?code=c&state=st&client_id=other", &p, "oaiapp_9").is_err());
    assert!(parse_callback("/favicon.ico", &p, "").is_err());
}

#[test]
fn tokens_without_plan_scope_are_refused() {
    let ok = r#"{"access_token":"a","refresh_token":"r","id_token":"i","expires_in":3600,"scope":"openid email offline_access resource.invoke chatgpt.tokens.use.direct"}"#;
    let t = parse_tokens(ok, 1000).unwrap();
    assert_eq!((t.expires_at, t.access_token.as_str()), (4600, "a"));
    let no_plan = ok.replace(" chatgpt.tokens.use.direct", "");
    assert_eq!(parse_tokens(&no_plan, 0).unwrap_err(), AiError::PlanNotEligible);
}

#[test]
fn id_token_gives_email_and_nonce() {
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"email":"me@x.org","nonce":"no"}"#);
    assert_eq!(id_token_claims(&format!("h.{payload}.s")), Some(("me@x.org".into(), "no".into())));
    assert_eq!(id_token_claims("garbage"), None);
}

#[test]
fn login_file_round_trips_privately_and_signs_out() {
    let dir = crate::testterm::temp_dir("login");
    let path = dir.join("chatgpt-login.json");
    let mut l = Login { client_id: "c".into(), host_id: "h".into(), email: "e".into(), refresh_token: "r".into(), access_token: "a".into(), expires_at: 100, scopes: vec!["chatgpt.tokens.use.direct".into()], welcomed: true, ..Login::default() };
    l.save(&path).unwrap();
    assert_eq!(Login::load(&path), l);
    #[cfg(unix)]
    assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&path).unwrap().permissions()) & 0o777, 0o600);
    assert!(l.signed_in() && l.needs_refresh(41) && !l.needs_refresh(39));
    l.sign_out();
    assert!(!l.signed_in());
    assert_eq!((l.client_id.as_str(), l.host_id.as_str(), l.welcomed, l.refresh_token.as_str()), ("c", "h", true, ""));
    std::fs::write(&path, "not json").unwrap();
    assert_eq!(Login::load(&path), Login::default());
}
```

- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** Percent-encode with RFC 3986 unreserved set (`%20` for space). **Step 4:** `cargo test`, clippy → PASS. **Step 5: Commit** `AI: ChatGPT sign-in pieces and the saved login`

---

### Task 3: Sign-in, refresh and revoke over the network; Client on a ChatGPT plan

**Files:** `src/ai/login.rs`, `src/ai/client.rs`, `src/ai/mod.rs`, `src/session.rs`

**Interfaces:**
- `pub fn exchange(agent: &ureq::Agent, auth: &str, client_id: &str, code: &str, p: &Pending) -> Result<Tokens, AiError>`
- `pub fn refresh(agent, auth: &str, login: &mut Login, now: u64) -> Result<(), AiError>` — on a refresh error code in `invalid_grant | invalid_refresh_token | token_expired | refresh_token_expired | refresh_token_invalidated | refresh_token_reused` → `login.sign_out()` and `Err(SignInAgain)`.
- `pub fn revoke(agent, auth: &str, login: &Login)` — GET `{auth}/.well-known/openid-configuration`, POST form to its `revocation_endpoint`; errors only logged.
- `pub fn sign_in(job: u64, proxy: EventLoopProxy<UserEvent>, cancel: Arc<AtomicBool>)` — thread `ai-login`: bind 1455 or 0; `Pending` with random state/nonce/verifier; `crate::open::open(authorize_url)`; accept (nonblocking poll every 200 ms, until `cancel` or 5 min); answer the browser with a small HTML page; `parse_callback`; `exchange`; nonce check via `id_token_claims`; save `Login` (email, tokens, issued client id, host id made once as `urn:uuid:` + 32 hex chars); send `AiEvent::SignedIn { job, result: Result<String /*email*/, AiError> }`.
- `pub fn sign_in_with(listener: TcpListener, auth: &str, path: &Path, open: impl FnOnce(&str), cancel: &AtomicBool) -> Result<String, AiError>` — the testable core of `sign_in`.
- A process-wide `static REFRESH: Mutex<()>` held while loading, refreshing and saving the login.
- `Client::new`: provider OpenAI and `Login::load(path).signed_in()` → `Client { format: Responses, base: api_base(), auth: Auth::ChatGpt(path) }`, else the API-key client as before. Before each request a ChatGPT client takes `REFRESH`, reloads the login, refreshes if `needs_refresh(now)`, saves, and sends `Authorization: Bearer <access_token>`. Responses requests read the body as a stream into `parse_responses_stream`.

- [ ] **Step 1: Failing tests** with local servers (use `testserver`; add `serve_paths(Vec<(&'static str /*path prefix*/, &'static str /*body*/)>)` if needed):

```rust
#[test]
fn sign_in_with_a_fake_openai() { /* fake auth answers POST /api/accounts/oauth/token with tokens whose id_token carries the nonce from the authorize URL;
   `open` parses state/nonce/redirect from the URL and does GET {redirect}?code=c&state={state}&client_id=oaiapp_1 on its own thread;
   assert Ok("me@x.org"), the saved Login has client_id "oaiapp_1", tokens, a urn:uuid host id; the token request body has grant_type=authorization_code, client_id=oaiapp_1, code_verifier=, resource= */ }

#[test]
fn an_expired_token_is_refreshed_first() { /* Login with expires_at 0 saved; fake auth answers the refresh, fake API answers /responses with a completed stream;
   Client::chatgpt_with(path, auth, api).send(..) succeeds; the API request carries the new access token; the file has the new tokens */ }

#[test]
fn a_dead_refresh_token_signs_out() { /* refresh answers 400 {"error":"invalid_grant"} → send() == Err(SignInAgain); the file has no tokens but keeps client_id */ }

#[test]
fn a_cancelled_sign_in_stops() { /* cancel set before any callback → Err(Cancelled) within a second */ }
```

(Write these out fully in the code; the comments say what each asserts.)

- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4:** `cargo test`, clippy → PASS. **Step 5: Commit** `AI: sign in to ChatGPT and run Fix on the plan`

---

### Task 4: Options → AI account row, welcome panel, links on Windows

**Files:** `src/chrome.rs`, `src/app.rs`, `src/open.rs`

**Interfaces:**
- `OptionsState` gains `ai_account: Option<String>` (email when signed in), `ai_signing_in: bool`.
- Actions: `AiSignIn`, `AiSignOut`, `AiManageUsage`, `AiWelcomeDone`.
- Page (provider ChatGPT only): row `Account` — signed out: button `Continue with ChatGPT` (or text `Waiting for the browser…` + button `Cancel` = `AiSignIn` again cancels); signed in: `{email} · Using ChatGPT plan`, buttons `Manage usage`, `Sign out`. When signed in the key row text becomes `not used while signed in` (no Paste/Remove).
- `Overlay::AiWelcome` → `chrome::ai_welcome_panel(m, offset)`: title `You're using your ChatGPT plan`, text "Fix now uses your ChatGPT plan's allowance instead of an API key. You can see and limit it in Manage usage.", buttons `Got it` (`AiWelcomeDone`) and `Manage usage`.
- App: `AiSignIn` starts `login::sign_in` (or cancels a waiting one); `AiEvent::SignedIn` Ok → clear `cfg.ai.model`, reload models, show the welcome panel if `!welcomed` (then save `welcomed = true` on Got it); Err → status. `AiSignOut` → `revoke` on a thread, `sign_out`, save, clear model, reload models. `AiManageUsage` → open `https://chatgpt.com/settings/usage`. Fix notice says `Asking ChatGPT (your plan)…` when signed in.
- `open.rs`: on Windows, a target starting `http://` or `https://` opens with `rundll32 url.dll,FileProtocolHandler <url>` (cmd would split it at `&`).

- [ ] **Step 1: Failing tests** (`chrome.rs`, `open.rs`)

```rust
#[test]
fn chatgpt_signed_out_offers_continue() {
    let s = OptionsState { page: OptionsPage::Ai, ai_provider: Provider::OpenAi, ..OptionsState::default() };
    let p = options_panel(&m(), &s, (0.0, 0.0));
    assert!(p.buttons.iter().any(|b| b.action == Action::AiSignIn && b.label == "Continue with ChatGPT"));
    let claude = OptionsState { ai_provider: Provider::Anthropic, ..s };
    assert!(!options_panel(&m(), &claude, (0.0, 0.0)).buttons.iter().any(|b| b.action == Action::AiSignIn));
}

#[test]
fn chatgpt_signed_in_shows_plan_and_sign_out() {
    let s = OptionsState { page: OptionsPage::Ai, ai_provider: Provider::OpenAi, ai_account: Some("me@x.org".into()), ..OptionsState::default() };
    let p = options_panel(&m(), &s, (0.0, 0.0));
    assert!(p.lines.iter().any(|l| l.text.contains("me@x.org · Using ChatGPT plan")));
    assert!(p.buttons.iter().any(|b| b.action == Action::AiManageUsage));
    assert!(p.buttons.iter().any(|b| b.action == Action::AiSignOut));
    assert!(!p.buttons.iter().any(|b| b.action == Action::AiPasteKey));
}

#[test]
fn welcome_panel_says_plan_and_got_it() {
    let p = ai_welcome_panel(&m(), (0.0, 0.0));
    assert!(p.buttons.iter().any(|b| b.action == Action::AiWelcomeDone && b.label == "Got it"));
}

// open.rs
#[test]
fn windows_urls_skip_cmd() {
    let o = windows_openers(std::ffi::OsStr::new("https://a.b/?x=1&y=2"));
    assert_eq!(o[0].0, "rundll32");
}
```

(`windows_openers` is the Windows branch of `openers` pulled out so it can be tested on every platform.)

- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4:** `cargo test`, clippy, `make check-all DEV_RUN='$(call docker_run,$(DEV_IMAGE),target/docker-dev)'` → PASS. Manual (Docker/Xvfb, debug build, `FASTCNSL_CHATGPT_AUTH`/`_API` → a fake server): Options → AI → ChatGPT → Continue with ChatGPT; a script plays the browser (GET on the redirect); welcome panel; Fix runs through `/v1/responses`. **Step 5: Commit** `AI: Continue with ChatGPT in Options → AI`

---

### Task 5: Docs

README "Fix a command with AI": ChatGPT sign-in (Plus or Pro; uses the plan's allowance; Manage usage; Sign out; `chatgpt-login.json`), and why Grok and Claude still need keys. CHANGELOG Unreleased entry amended. Spec "Stages" list order fixed (1, 1b, 1c). Commit `AI: README and changelog for ChatGPT sign-in`.
