//! HTTPS to the providers. Blocking: every call runs on a thread of its
//! own, never on the UI thread.

use std::io::BufReader;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::grok::{self, GrokLogin};
use super::login::{self, Login};
use super::openrouter;
use super::wire::{self, Format, Reply, Request};
use super::{AiConfig, AiError, Keys, Provider};

const ANTHROPIC_BASE: &str = "https://api.anthropic.com/v1";
const OPENAI_BASE: &str = "https://api.openai.com/v1";
const XAI_BASE: &str = "https://api.x.ai/v1";
const ANTHROPIC_VERSION: &str = "2026-10-21";
/// Largest reply read; model lists and answers are far smaller.
const MAX_REPLY: u64 = 4 * 1024 * 1024;

/// How requests prove who is asking.
enum Auth {
    /// An API key ("" for a local server that needs none).
    Key(String),
    /// A ChatGPT sign-in: the saved login (refreshed when it is about to
    /// expire) and the sign-in server.
    ChatGpt { path: PathBuf, auth: String },
    /// A Grok sign-in, likewise.
    Grok { path: PathBuf, auth: String },
    /// The user's Claude Code, found when first needed (on the request's
    /// thread: finding it in WSL takes a moment).
    ClaudeCode(Option<super::claude_code::Launcher>),
}

/// A connection to one provider.
pub struct Client {
    provider: Provider,
    auth: Auth,
    base: String,
    agent: ureq::Agent,
}

impl Client {
    /// The client the settings and keys describe. With ChatGPT, a sign-in
    /// comes before the API key: requests then spend the user's plan.
    pub fn new(cfg: &AiConfig, keys: &Keys) -> Result<Client, AiError> {
        let p = cfg.provider;
        if p == Provider::OpenAi
            && let Some(path) = Login::path()
            && Login::load(&path).signed_in()
        {
            return Ok(Client::chatgpt_with(path, &login::auth_base(), &login::api_base()));
        }
        if p == Provider::Xai
            && let Some(path) = GrokLogin::path()
            && GrokLogin::load(&path).signed_in()
        {
            return Ok(Client::grok_with(path, &grok::auth_base(), &grok::api_base()));
        }
        if p == Provider::ClaudeCode {
            return Ok(Client { provider: p, auth: Auth::ClaudeCode(None), base: String::new(), agent: agent(Duration::from_secs(10)) });
        }
        let base = match p {
            Provider::Off => return Err(AiError::NoProvider),
            Provider::ClaudeCode => unreachable!("handled above"),
            Provider::OpenAi => OPENAI_BASE,
            Provider::Xai => XAI_BASE,
            Provider::Anthropic => ANTHROPIC_BASE,
            Provider::OpenRouter => &openrouter::api_base(&openrouter::site()),
            Provider::Custom if cfg.base_url.trim().is_empty() => return Err(AiError::NoAddress),
            Provider::Custom => cfg.base_url.trim(),
        };
        // A local server usually needs no key.
        let key = match keys.get(p) {
            Some(k) => k,
            None if p == Provider::Custom => String::new(),
            None => return Err(AiError::NoKey),
        };
        Ok(Client::with_base(p, &key, base))
    }

    pub fn with_base(provider: Provider, key: &str, base: &str) -> Client {
        Client { provider, auth: Auth::Key(key.to_owned()), base: base.trim_end_matches('/').to_owned(), agent: agent(Duration::from_secs(120)) }
    }

    /// A client spending the ChatGPT plan of the login saved at `path`.
    pub fn chatgpt_with(path: PathBuf, auth: &str, api: &str) -> Client {
        Client {
            provider: Provider::OpenAi,
            auth: Auth::ChatGpt { path, auth: auth.trim_end_matches('/').to_owned() },
            base: api.trim_end_matches('/').to_owned(),
            agent: agent(Duration::from_secs(120)),
        }
    }

    /// A client spending the Grok plan of the login saved at `path`.
    pub fn grok_with(path: PathBuf, auth: &str, api: &str) -> Client {
        Client {
            provider: Provider::Xai,
            auth: Auth::Grok { path, auth: auth.trim_end_matches('/').to_owned() },
            base: api.trim_end_matches('/').to_owned(),
            agent: agent(Duration::from_secs(120)),
        }
    }

    /// A client running `launcher` (tests).
    #[cfg(test)]
    pub fn claude_code_with(launcher: super::claude_code::Launcher) -> Client {
        Client { provider: Provider::ClaudeCode, auth: Auth::ClaudeCode(Some(launcher)), base: String::new(), agent: agent(Duration::from_secs(10)) }
    }

    /// Claude Code: the one given, or the one installed.
    fn claude_code(&self) -> Result<super::claude_code::Launcher, AiError> {
        match &self.auth {
            Auth::ClaudeCode(Some(l)) => Ok(l.clone()),
            _ => super::claude_code::find(),
        }
    }

    /// Requests spend a ChatGPT or Grok plan.
    pub fn on_plan(&self) -> bool {
        matches!(self.auth, Auth::ChatGpt { .. } | Auth::Grok { .. })
    }

    /// The API format requests use.
    pub fn format(&self) -> Format {
        match &self.auth {
            Auth::ChatGpt { .. } | Auth::Grok { .. } => Format::Responses,
            Auth::ClaudeCode(_) => Format::ClaudeCode,
            Auth::Key(_) if self.provider.uses_messages_api() => Format::Messages,
            Auth::Key(_) => Format::Chat,
        }
    }

    /// Asks one question.
    pub fn send(&self, r: &Request) -> Result<Reply, AiError> {
        let format = self.format();
        let (path, body) = match format {
            Format::Messages => ("/messages", wire::messages_body(r)),
            Format::Chat => ("/chat/completions", wire::chat_body(r)),
            Format::Responses => ("/responses", wire::responses_body(r)),
            Format::ClaudeCode => return super::claude_code::send(&self.claude_code()?, r),
        };
        let response = self.request(path, Some(&body.to_string()))?;
        if format == Format::Responses {
            // Always streamed; read to its last event.
            return wire::parse_responses_stream(BufReader::new(response.into_body().into_reader()));
        }
        let text = read_text(response)?;
        if format == Format::Messages { wire::parse_messages(&text) } else { wire::parse_chat(&text) }
    }

    /// The models the key (or plan) can use.
    pub fn models(&self) -> Result<Vec<String>, AiError> {
        if self.format() == Format::ClaudeCode {
            // Its aliases; finding it tells the user early when it's missing.
            self.claude_code()?;
            return Ok(super::claude_code::MODELS.iter().map(|m| (*m).to_owned()).collect());
        }
        if self.provider == Provider::OpenRouter {
            // Only the models that can call tools: Fix needs them.
            let ids = wire::parse_models(&read_text(self.request("/models?supported_parameters=tools", None)?)?)?;
            return Ok(openrouter::order_models(ids));
        }
        let path = if self.format() == Format::Messages { "/models?limit=1000" } else { "/models" };
        wire::parse_models(&read_text(self.request(path, None)?)?)
    }

    /// GET (no body) or POST `body` to `path`; the response when the status
    /// is 2xx. A connection dropped before any answer (a pooled connection
    /// the server closed) is tried once more.
    fn request(&self, path: &str, body: Option<&str>) -> Result<ureq::http::Response<ureq::Body>, AiError> {
        let mut header = self.auth_header(false)?;
        let started = Instant::now();
        let mut tries = 0;
        let mut refreshed = false;
        let result = loop {
            tries += 1;
            match self.request_once(path, body, &header) {
                Err(ureq::Error::Io(e)) if tries == 1 && dropped(&e) => continue,
                // A Grok token xAI stopped taking before its time: once more
                // with a fresh one.
                Ok(r) if r.status().as_u16() == 401 && matches!(self.auth, Auth::Grok { .. }) && !refreshed => {
                    refreshed = true;
                    header = self.auth_header(true)?;
                }
                r => break r,
            }
        };
        let url_path = path.split('?').next().unwrap_or(path);
        match result {
            Ok(response) => {
                let status = response.status().as_u16();
                // Never the key or the bodies: they hold secrets and terminal text.
                log::info!("AI: {} {url_path}: {status} in {} ms", self.provider.name(), started.elapsed().as_millis());
                if (200..300).contains(&status) {
                    return Ok(response);
                }
                let text = read_text(response).unwrap_or_default();
                if status == 403 && matches!(self.auth, Auth::Grok { .. }) {
                    // xAI's plan gate: an error body has no secrets.
                    log::warn!("AI: Grok plan refused: {}", text.chars().take(500).collect::<String>());
                    return Err(AiError::GrokPlan);
                }
                if self.provider == Provider::OpenRouter
                    && matches!(status, 402 | 429)
                    && let Some(message) = openrouter::error_message(&text)
                {
                    // Its reason says what to do (the free models' daily
                    // limit, credits to add).
                    return Err(AiError::Api { status, message });
                }
                Err(wire::error_for(status, &text))
            }
            Err(e) => {
                log::info!("AI: {} {url_path}: {e}", self.provider.name());
                Err(AiError::Network(e.to_string()))
            }
        }
    }

    fn request_once(&self, path: &str, body: Option<&str>, header: &Option<(&'static str, String)>) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        let url = format!("{}{path}", self.base);
        let anthropic = self.format() == Format::Messages;
        let named: &[(&str, &str)] = if self.provider == Provider::OpenRouter { &openrouter::ATTRIBUTION } else { &[] };
        match body {
            Some(b) => {
                let mut r = self.agent.post(&url).header("content-type", "application/json");
                if let Some((name, value)) = header {
                    r = r.header(*name, value);
                }
                for (name, value) in named {
                    r = r.header(*name, *value);
                }
                if anthropic {
                    r = r.header("anthropic-version", ANTHROPIC_VERSION);
                }
                r.send(b)
            }
            None => {
                let mut r = self.agent.get(&url);
                if let Some((name, value)) = header {
                    r = r.header(*name, value);
                }
                for (name, value) in named {
                    r = r.header(*name, *value);
                }
                if anthropic {
                    r = r.header("anthropic-version", ANTHROPIC_VERSION);
                }
                r.call()
            }
        }
    }

    /// The header that carries the key or the plan's access token (None:
    /// no key). A plan's token about to expire (or any Grok token, with
    /// `force`) is refreshed first, one refresh at a time across threads.
    fn auth_header(&self, force: bool) -> Result<Option<(&'static str, String)>, AiError> {
        match &self.auth {
            Auth::ClaudeCode(_) => Ok(None),
            Auth::Key(k) if k.is_empty() => Ok(None),
            Auth::Key(k) if self.provider.uses_messages_api() => Ok(Some(("x-api-key", k.clone()))),
            Auth::Key(k) => Ok(Some(("authorization", format!("Bearer {k}")))),
            Auth::ChatGpt { path, auth } => {
                let _one = login::LOCK.lock().unwrap_or_else(|e| e.into_inner());
                let mut l = Login::load(path);
                if !l.signed_in() {
                    return Err(AiError::SignInAgain);
                }
                let now = login::now();
                if l.needs_refresh(now) {
                    let refreshed = login::refresh(&login::agent(), auth, &mut l, now);
                    // Saved either way: a failed refresh may have signed out.
                    if let Err(e) = l.save(path) {
                        log::warn!("could not save the ChatGPT sign-in: {e}");
                    }
                    refreshed?;
                }
                Ok(Some(("authorization", format!("Bearer {}", l.access_token))))
            }
            Auth::Grok { path, auth } => {
                let _one = login::LOCK.lock().unwrap_or_else(|e| e.into_inner());
                let mut l = GrokLogin::load(path);
                if !l.signed_in() {
                    return Err(AiError::SignInAgain);
                }
                let now = login::now();
                if force || l.needs_refresh(now) {
                    let refreshed = grok::refresh(&login::agent(), auth, &mut l, now);
                    // Saved either way: a failed refresh may have signed out.
                    if let Err(e) = l.save(path) {
                        log::warn!("could not save the Grok sign-in: {e}");
                    }
                    refreshed?;
                }
                Ok(Some(("authorization", format!("Bearer {}", l.access_token))))
            }
        }
    }
}

/// An agent that leaves non-2xx statuses to us, with these timeouts.
pub fn agent(total: Duration) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(total))
        .build();
    ureq::Agent::new_with_config(config)
}

/// A response's body as text, at most `MAX_REPLY` bytes.
fn read_text(mut response: ureq::http::Response<ureq::Body>) -> Result<String, AiError> {
    response.body_mut().with_config().limit(MAX_REPLY).read_to_string().map_err(|e| AiError::Network(e.to_string()))
}

/// Debug output leaves the key out.
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("provider", &self.provider).field("base", &self.base).finish_non_exhaustive()
    }
}

/// The connection broke before an answer came: worth one more try.
fn dropped(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::*;
    matches!(e.kind(), ConnectionReset | ConnectionAborted | BrokenPipe | UnexpectedEof)
}

/// A local server playing the provider, for tests.
#[cfg(test)]
pub mod testserver {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    /// Reads one request (headers and Content-Length bytes) from `s`.
    fn read_request(s: &mut TcpStream) -> String {
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        let body_start = loop {
            let n = s.read(&mut buf).unwrap();
            raw.extend_from_slice(&buf[..n]);
            if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
        };
        let head = String::from_utf8_lossy(&raw[..body_start]).to_lowercase();
        let len = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
            .unwrap_or(0);
        while raw.len() < body_start + len {
            let n = s.read(&mut buf).unwrap();
            raw.extend_from_slice(&buf[..n]);
        }
        String::from_utf8_lossy(&raw).into_owned()
    }

    /// Writes a response; a 302's `body` is where it points.
    fn answer(s: &mut TcpStream, status: u16, body: &str) {
        if status == 302 {
            let reply = format!("HTTP/1.1 302 Found\r\nlocation: {body}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
            s.write_all(reply.as_bytes()).unwrap();
            return;
        }
        let reply = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        s.write_all(reply.as_bytes()).unwrap();
    }

    /// Answers one request with `status` and `body`; returns the base URL
    /// and the raw request it got.
    pub fn serve_once(status: u16, body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let raw = read_request(&mut s);
            answer(&mut s, status, body);
            raw
        });
        (base, handle)
    }

    /// Answers requests with `bodies` (status 200) in turn, one per
    /// connection; returns the base URL and the raw requests.
    pub fn serve(bodies: Vec<&'static str>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let mut bodies = bodies.into_iter();
        let n = bodies.len();
        serve_with(n, move |_| (200, bodies.next().unwrap_or("").to_owned()))
    }

    /// Answers `count` requests, each with what `answer` makes of the raw
    /// request (status, body); returns the base URL and the raw requests.
    pub fn serve_with(count: usize, mut answer_with: impl FnMut(&str) -> (u16, String) + Send + 'static) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            (0..count)
                .map(|_| {
                    let (mut s, _) = listener.accept().unwrap();
                    let raw = read_request(&mut s);
                    let (status, body) = answer_with(&raw);
                    answer(&mut s, status, &body);
                    raw
                })
                .collect()
        });
        (base, handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::wire::Request;

    use super::testserver::{serve_once, serve_with};

    fn req() -> Request<'static> {
        static U: std::sync::LazyLock<[serde_json::Value; 1]> = std::sync::LazyLock::new(|| [crate::ai::wire::user_message("u")]);
        Request { model: "m", system: "s", messages: &*U, tools: &[], max_tokens: 10 }
    }

    #[test]
    fn claude_request_goes_to_messages_with_its_headers() {
        let (base, got) = serve_once(200, r#"{"content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn"}"#);
        let c = Client::with_base(Provider::Anthropic, "k1", &base);
        assert_eq!(c.send(&req()).unwrap().text, "ok");
        let raw = got.join().unwrap().to_lowercase();
        assert!(raw.starts_with("post /messages "), "{raw}");
        assert!(raw.contains("x-api-key: k1") && raw.contains("anthropic-version: 2026-10-21"), "{raw}");
        assert!(!raw.contains("authorization:"));
    }

    #[test]
    fn chat_request_uses_bearer_and_maps_errors() {
        let (base, got) = serve_once(401, r#"{"error":{"message":"bad key"}}"#);
        let c = Client::with_base(Provider::OpenAi, "k2", &base);
        assert_eq!(c.send(&req()).unwrap_err(), AiError::BadKey);
        let raw = got.join().unwrap().to_lowercase();
        assert!(raw.starts_with("post /chat/completions ") && raw.contains("authorization: bearer k2"), "{raw}");
    }

    #[test]
    fn custom_without_a_key_sends_no_auth() {
        let (base, got) = serve_once(200, r#"{"choices":[{"message":{"content":"hi"},"finish_reason":"stop"}]}"#);
        let c = Client::with_base(Provider::Custom, "", &format!("{base}/"));
        assert_eq!(c.send(&req()).unwrap().text, "hi");
        let raw = got.join().unwrap().to_lowercase();
        assert!(raw.starts_with("post /chat/completions ") && !raw.contains("authorization:"), "{raw}");
    }

    #[test]
    fn models_are_listed_and_no_server_is_a_network_error() {
        let (base, got) = serve_once(200, r#"{"data":[{"id":"x"}]}"#);
        assert_eq!(Client::with_base(Provider::Anthropic, "k", &base).models().unwrap(), vec!["x"]);
        assert!(got.join().unwrap().to_lowercase().starts_with("get /models?limit=1000 "));
        let dead = Client::with_base(Provider::Xai, "k", "http://127.0.0.1:9");
        assert!(matches!(dead.models(), Err(AiError::Network(_))));
    }

    #[test]
    fn openrouter_names_fastcnsl_lists_tool_models_and_says_why_it_is_limited() {
        let (base, got) = serve_once(200, r#"{"data":[{"id":"a/paid"},{"id":"b/x:free"}]}"#);
        let c = Client::with_base(Provider::OpenRouter, "sk-or", &base);
        assert_eq!(c.models().unwrap(), ["openrouter/free", "b/x:free", "a/paid"]);
        let raw = got.join().unwrap().to_lowercase();
        assert!(raw.starts_with("get /models?supported_parameters=tools "), "{raw}");
        assert!(raw.contains("authorization: bearer sk-or") && raw.contains("x-openrouter-title: fastcnsl") && raw.contains("http-referer: https://"), "{raw}");

        let (base, got) = serve_once(429, r#"{"error":{"message":"Rate limit exceeded: free-models-per-day","code":429}}"#);
        let e = Client::with_base(Provider::OpenRouter, "sk-or", &base).send(&req()).unwrap_err();
        assert_eq!(e, AiError::Api { status: 429, message: "Rate limit exceeded: free-models-per-day".into() });
        assert!(got.join().unwrap().to_lowercase().starts_with("post /chat/completions "));
    }

    const COMPLETED: &str = "data: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"ok\"}]}]}}\n\n";

    /// A Grok login saved in a new folder, its token good for an hour.
    fn grok_login(name: &str) -> PathBuf {
        let path = crate::testterm::temp_dir(name).join("grok-login.json");
        let l = super::super::grok::GrokLogin { email: "e".into(), access_token: "a1".into(), refresh_token: "r1".into(), expires_at: login::now() + 3600 };
        l.save(&path).unwrap();
        path
    }

    #[test]
    fn a_grok_plan_sends_responses_with_its_token() {
        let path = grok_login("grok-client");
        let (base, got) = serve_with(1, |_| (200, COMPLETED.to_owned()));
        let c = Client::grok_with(path, "http://127.0.0.1:9", &base);
        assert!(c.on_plan());
        assert_eq!(c.send(&req()).unwrap().text, "ok");
        let raw = got.join().unwrap()[0].to_lowercase();
        assert!(raw.starts_with("post /responses ") && raw.contains("authorization: bearer a1"), "{raw}");
    }

    #[test]
    fn a_grok_401_refreshes_the_token_and_tries_once_more() {
        let path = grok_login("grok-401");
        let mut asked = 0;
        let (base, got) = serve_with(3, move |raw: &str| {
            if raw.starts_with("POST /oauth2/token") {
                return (200, r#"{"access_token":"a2","refresh_token":"r2","expires_in":21600}"#.to_owned());
            }
            asked += 1;
            if asked == 1 { (401, r#"{"error":"The OAuth2 access token could not be validated."}"#.to_owned()) } else { (200, COMPLETED.to_owned()) }
        });
        let c = Client::grok_with(path.clone(), &base, &base);
        assert_eq!(c.send(&req()).unwrap().text, "ok");
        let raws = got.join().unwrap();
        assert!(raws[2].to_lowercase().contains("authorization: bearer a2"), "{}", raws[2]);
        let l = super::super::grok::GrokLogin::load(&path);
        assert_eq!((l.access_token.as_str(), l.refresh_token.as_str()), ("a2", "r2"));
    }

    #[test]
    fn a_grok_403_is_the_plan_gate() {
        let path = grok_login("grok-403");
        let (base, got) = serve_with(1, |_| (403, r#"{"code":"x","error":"does not have permission to execute the specified operation"}"#.to_owned()));
        let c = Client::grok_with(path, "http://127.0.0.1:9", &base);
        assert_eq!(c.send(&req()).unwrap_err(), AiError::GrokPlan);
        got.join().unwrap();
    }

    #[test]
    fn a_grok_token_about_to_expire_is_refreshed_first() {
        let path = grok_login("grok-expiring");
        let mut l = super::super::grok::GrokLogin::load(&path);
        l.expires_at = login::now() + 60;
        l.save(&path).unwrap();
        let (base, got) = serve_with(2, |raw: &str| {
            if raw.starts_with("POST /oauth2/token") { (200, r#"{"access_token":"a3","expires_in":21600}"#.to_owned()) } else { (200, COMPLETED.to_owned()) }
        });
        let c = Client::grok_with(path, &base, &base);
        assert_eq!(c.send(&req()).unwrap().text, "ok");
        let raws = got.join().unwrap();
        assert!(raws[0].starts_with("POST /oauth2/token") && raws[1].to_lowercase().contains("authorization: bearer a3"));
    }

    #[test]
    fn new_needs_a_provider_and_a_key() {
        let keys = Keys::default();
        assert_eq!(Client::new(&AiConfig::default(), &keys).unwrap_err(), AiError::NoProvider);
        let cfg = AiConfig { provider: Provider::OpenAi, ..AiConfig::default() };
        // OPENAI_API_KEY may be set where tests run: only check when it isn't.
        if std::env::var_os("OPENAI_API_KEY").is_none() {
            assert_eq!(Client::new(&cfg, &keys).unwrap_err(), AiError::NoKey);
        }
        let custom = AiConfig { provider: Provider::Custom, ..AiConfig::default() };
        assert_eq!(Client::new(&custom, &keys).unwrap_err(), AiError::NoAddress, "custom needs base_url");
        let custom = AiConfig { provider: Provider::Custom, base_url: "http://localhost:1/v1".into(), ..AiConfig::default() };
        assert!(Client::new(&custom, &keys).is_ok(), "custom works without a key");
    }
}
