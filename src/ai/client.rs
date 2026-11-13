//! HTTPS to the providers. Blocking: every call runs on a thread of its
//! own, never on the UI thread.

use std::time::{Duration, Instant};

use super::wire::{self, Reply, Request};
use super::{AiConfig, AiError, Keys, Provider};

const ANTHROPIC_BASE: &str = "https://api.anthropic.com/v1";
const OPENAI_BASE: &str = "https://api.openai.com/v1";
const XAI_BASE: &str = "https://api.x.ai/v1";
const ANTHROPIC_VERSION: &str = "2026-10-21";
/// Largest reply read; model lists and answers are far smaller.
const MAX_REPLY: u64 = 4 * 1024 * 1024;

/// A connection to one provider with one key.
pub struct Client {
    provider: Provider,
    key: String,
    base: String,
    agent: ureq::Agent,
}

impl Client {
    /// The client the settings and keys describe.
    pub fn new(cfg: &AiConfig, keys: &Keys) -> Result<Client, AiError> {
        let p = cfg.provider;
        let base = match p {
            Provider::Off => return Err(AiError::NoProvider),
            Provider::OpenAi => OPENAI_BASE,
            Provider::Xai => XAI_BASE,
            Provider::Anthropic => ANTHROPIC_BASE,
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
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(120)))
            .build();
        Client { provider, key: key.to_owned(), base: base.trim_end_matches('/').to_owned(), agent: ureq::Agent::new_with_config(config) }
    }

    pub fn provider(&self) -> Provider {
        self.provider
    }

    /// Asks one question.
    pub fn send(&self, r: &Request) -> Result<Reply, AiError> {
        let messages = self.provider.uses_messages_api();
        let (path, body) = if messages { ("/messages", wire::messages_body(r)) } else { ("/chat/completions", wire::chat_body(r)) };
        let body = body.to_string();
        let text = self.request(path, Some(&body))?;
        if messages { wire::parse_messages(&text) } else { wire::parse_chat(&text) }
    }

    /// The models the key can use.
    pub fn models(&self) -> Result<Vec<String>, AiError> {
        let path = if self.provider.uses_messages_api() { "/models?limit=1000" } else { "/models" };
        wire::parse_models(&self.request(path, None)?)
    }

    /// GET (no body) or POST `body` to `path`; the reply's text when the
    /// status is 2xx. A connection dropped before any answer (a pooled
    /// connection the server closed) is tried once more.
    fn request(&self, path: &str, body: Option<&str>) -> Result<String, AiError> {
        let started = Instant::now();
        let mut tries = 0;
        let result = loop {
            tries += 1;
            match self.request_once(path, body) {
                Err(ureq::Error::Io(e)) if tries == 1 && dropped(&e) => continue,
                r => break r,
            }
        };
        let url_path = path.split('?').next().unwrap_or(path);
        match result {
            Ok((status, text)) => {
                // Never the key or the bodies: they hold secrets and terminal text.
                log::info!("AI: {} {url_path}: {status} in {} ms", self.provider.name(), started.elapsed().as_millis());
                if (200..300).contains(&status) { Ok(text) } else { Err(wire::error_for(status, &text)) }
            }
            Err(e) => {
                log::info!("AI: {} {url_path}: {e}", self.provider.name());
                Err(AiError::Network(e.to_string()))
            }
        }
    }

    fn request_once(&self, path: &str, body: Option<&str>) -> Result<(u16, String), ureq::Error> {
        let url = format!("{}{path}", self.base);
        let mut response = match body {
            Some(b) => self.authorize_post(self.agent.post(&url)).header("content-type", "application/json").send(b)?,
            None => self.authorize_get(self.agent.get(&url)).call()?,
        };
        let status = response.status().as_u16();
        let text = response.body_mut().with_config().limit(MAX_REPLY).read_to_string()?;
        Ok((status, text))
    }

    fn authorize_post(&self, r: ureq::RequestBuilder<ureq::typestate::WithBody>) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        let (name, value) = self.auth();
        let r = if name.is_empty() { r } else { r.header(name, &value) };
        if self.provider.uses_messages_api() { r.header("anthropic-version", ANTHROPIC_VERSION) } else { r }
    }

    fn authorize_get(&self, r: ureq::RequestBuilder<ureq::typestate::WithoutBody>) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
        let (name, value) = self.auth();
        let r = if name.is_empty() { r } else { r.header(name, &value) };
        if self.provider.uses_messages_api() { r.header("anthropic-version", ANTHROPIC_VERSION) } else { r }
    }

    /// The header that carries the key; ("", "") without a key.
    fn auth(&self) -> (&'static str, String) {
        if self.key.is_empty() {
            ("", String::new())
        } else if self.provider.uses_messages_api() {
            ("x-api-key", self.key.clone())
        } else {
            ("authorization", format!("Bearer {}", self.key))
        }
    }
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

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;
    use crate::ai::wire::Request;

    /// Answers one request with `status` and `body`; returns the base URL
    /// and the raw request it got.
    fn serve_once(status: u16, body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            // Headers, then Content-Length bytes of body.
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
            let reply = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(reply.as_bytes()).unwrap();
            String::from_utf8_lossy(&raw).into_owned()
        });
        (base, handle)
    }

    fn req() -> Request<'static> {
        Request { model: "m", system: "s", user: "u", tools: &[], max_tokens: 10 }
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
