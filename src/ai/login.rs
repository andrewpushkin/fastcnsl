//! "Sign in with ChatGPT": an open-source app may spend a ChatGPT Plus or
//! Pro plan instead of an API key (developers.openai.com/siwc). The sign-in
//! is OAuth with PKCE, the browser coming back to a listener on
//! 127.0.0.1; the tokens are kept in `chatgpt-login.json` beside the
//! settings file, readable by the user only.

use std::io;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::AiError;
use crate::config::Config;

/// OpenAI's sign-in server.
pub const AUTH: &str = "https://auth.openai.com";
/// OpenAI's API, where a plan's requests go.
pub const API: &str = "https://api.openai.com/v1";
/// The scopes asked for: who the user is, and spending their plan.
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
/// The scope without which the plan can't be used.
pub const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
/// The client id that asks OpenAI to register this app on first sign-in.
const REGISTER: &str = "dynamic_agent_client";
/// Where the browser comes back to (the port may change).
pub const CALLBACK_PATH: &str = "/auth/callback";

/// The sign-in server, or in debug builds `FASTCNSL_CHATGPT_AUTH` (tests
/// against a fake server).
pub fn auth_base() -> String {
    debug_override("FASTCNSL_CHATGPT_AUTH").unwrap_or_else(|| AUTH.to_owned())
}

/// The API, or in debug builds `FASTCNSL_CHATGPT_API`.
pub fn api_base() -> String {
    debug_override("FASTCNSL_CHATGPT_API").unwrap_or_else(|| API.to_owned())
}

fn debug_override(var: &str) -> Option<String> {
    if cfg!(debug_assertions) { std::env::var(var).ok().filter(|v| !v.is_empty()) } else { None }
}

/// What is kept between runs. Sign-out clears the account and tokens but
/// keeps the registration (`client_id`) and this computer's id.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Login {
    /// The client id OpenAI issued at the first sign-in.
    pub client_id: String,
    /// This computer's id for OpenAI (`urn:uuid:…`), made once.
    pub host_id: String,
    pub email: String,
    /// Kept as a hint for the next sign-in.
    pub id_token: String,
    pub access_token: String,
    pub refresh_token: String,
    /// When the access token expires, in seconds since 1970.
    pub expires_at: u64,
    pub scopes: Vec<String>,
    /// The "You're using your ChatGPT plan" panel was shown.
    pub welcomed: bool,
}

impl Login {
    /// `chatgpt-login.json` beside the settings file.
    pub fn path() -> Option<PathBuf> {
        Some(Config::path()?.with_file_name("chatgpt-login.json"))
    }

    /// Reads `path`; a missing or broken file is a login with nothing in it.
    pub fn load(path: &Path) -> Login {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|_| {
                log::warn!("{} is not valid: signed out of ChatGPT", path.display());
                Login::default()
            }),
            Err(_) => Login::default(),
        }
    }

    /// Writes the whole file under another name, then renames it, so a
    /// crash never leaves half a file; readable by the user only.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        crate::config::write_private(&tmp, &text)?;
        std::fs::rename(&tmp, path)
    }

    /// Signed in, with the right to spend the plan.
    pub fn signed_in(&self) -> bool {
        !self.refresh_token.is_empty() && self.scopes.iter().any(|s| s == PLAN_SCOPE)
    }

    /// The access token expires within a minute of `now`.
    pub fn needs_refresh(&self, now: u64) -> bool {
        now + 60 >= self.expires_at
    }

    pub fn sign_out(&mut self) {
        *self = Login { client_id: std::mem::take(&mut self.client_id), host_id: std::mem::take(&mut self.host_id), welcomed: self.welcomed, ..Login::default() };
    }

    /// Takes `t`, replacing the tokens together.
    pub fn take_tokens(&mut self, t: Tokens) {
        self.access_token = t.access_token;
        if !t.refresh_token.is_empty() {
            self.refresh_token = t.refresh_token;
        }
        if !t.id_token.is_empty() {
            self.id_token = t.id_token;
        }
        self.expires_at = t.expires_at;
        self.scopes = t.scopes;
    }
}

/// `bytes` random bytes from the system, as URL-safe base64.
pub fn random_token(bytes: usize) -> String {
    use ring::rand::SecureRandom;
    let mut buf = vec![0u8; bytes];
    // The system's random source failing is not something to go on from.
    ring::rand::SystemRandom::new().fill(&mut buf).expect("system random source");
    URL_SAFE_NO_PAD.encode(buf)
}

/// A new id for this computer: `urn:uuid:` and a random version-4 UUID.
pub fn new_host_id() -> String {
    use ring::rand::SecureRandom;
    let mut b = [0u8; 16];
    ring::rand::SystemRandom::new().fill(&mut b).expect("system random source");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("urn:uuid:{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// The PKCE challenge for `verifier`: its SHA-256 as URL-safe base64.
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()))
}

/// One sign-in's secrets, checked when the browser comes back.
pub struct Pending {
    pub state: String,
    pub nonce: String,
    pub verifier: String,
    pub redirect: String,
}

impl Pending {
    pub fn new(redirect: String) -> Pending {
        Pending { state: random_token(24), nonce: random_token(24), verifier: random_token(48), redirect }
    }
}

/// Percent-encodes everything but RFC 3986's unreserved characters.
pub fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A form body (`a=1&b=2`) from pairs.
pub fn form(pairs: &[(&str, &str)]) -> String {
    pairs.iter().map(|(k, v)| format!("{}={}", encode(k), encode(v))).collect::<Vec<_>>().join("&")
}

/// The address the browser opens to sign in. The first time it asks OpenAI
/// to register fastcnsl; after that it uses the issued client id and the
/// last ID token as a hint.
pub fn authorize_url(auth: &str, login: &Login, p: &Pending) -> String {
    let challenge = challenge(&p.verifier);
    let mut q: Vec<(&str, &str)> = vec![("response_type", "code")];
    if login.client_id.is_empty() {
        q.push(("client_id", REGISTER));
        q.push(("agent_name_hint", "fastcnsl"));
    } else {
        q.push(("client_id", &login.client_id));
        if !login.id_token.is_empty() {
            q.push(("id_token_hint", &login.id_token));
        }
    }
    q.extend([
        ("ext_agent_host_id", login.host_id.as_str()),
        ("redirect_uri", p.redirect.as_str()),
        ("scope", SCOPE),
        ("resource", API),
        ("state", p.state.as_str()),
        ("nonce", p.nonce.as_str()),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
    ]);
    format!("{auth}/api/accounts/authorize?{}", form(&q))
}

/// Checks the browser's return to `target` (path and query): the code and
/// the client id (issued now, or the one saved, `saved_client`).
pub fn parse_callback(target: &str, p: &Pending, saved_client: &str) -> Result<(String, String), String> {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != CALLBACK_PATH {
        return Err("not the sign-in callback".into());
    }
    let get = |name: &str| query.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| decode(v));
    if get("state").as_deref() != Some(p.state.as_str()) {
        return Err("the sign-in came back with the wrong state".into());
    }
    if let Some(e) = get("error") {
        return Err(if e == "access_denied" { "sign-in was cancelled".into() } else { format!("sign-in failed: {e}") });
    }
    let code = get("code").filter(|c| !c.is_empty()).ok_or("the sign-in brought no code")?;
    let client = match (get("client_id"), saved_client) {
        (Some(c), "") => c,
        (Some(c), saved) if c == saved => c,
        (Some(_), _) => return Err("the sign-in came back for another app registration".into()),
        (None, "") => return Err("the sign-in brought no client id".into()),
        (None, saved) => saved.to_owned(),
    };
    Ok((code, client))
}

/// Tokens from the token endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub expires_at: u64,
    pub scopes: Vec<String>,
}

/// Reads a token response; `now` in seconds since 1970. Without the plan
/// scope the plan can't be used.
pub fn parse_tokens(body: &str, now: u64) -> Result<Tokens, AiError> {
    parse_tokens_with(body, now, &[])
}

/// As [`parse_tokens`]; a response without `scope` keeps `scopes` (a
/// refresh that leaves the grant as it was).
fn parse_tokens_with(body: &str, now: u64, scopes: &[String]) -> Result<Tokens, AiError> {
    let v: Value = serde_json::from_str(body).map_err(|e| AiError::BadReply(format!("tokens: {e}")))?;
    let s = |k: &str| v[k].as_str().unwrap_or("").to_owned();
    let access_token = s("access_token");
    if access_token.is_empty() {
        return Err(AiError::BadReply("no access token".into()));
    }
    let scopes: Vec<String> = match v["scope"].as_str() {
        Some(s) => s.split_whitespace().map(str::to_owned).collect(),
        None => scopes.to_vec(),
    };
    if !scopes.iter().any(|s| s == PLAN_SCOPE) {
        return Err(AiError::PlanNotEligible);
    }
    Ok(Tokens { access_token, refresh_token: s("refresh_token"), id_token: s("id_token"), expires_at: now + v["expires_in"].as_u64().unwrap_or(3600), scopes })
}

/// The email and nonce in an ID token's payload. The signature is not
/// checked: the token only shows who is signed in and is never used to
/// authorise anything.
pub fn id_token_claims(id_token: &str) -> Option<(String, String)> {
    let payload = id_token.split('.').nth(1)?;
    let v: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?).ok()?;
    Some((v["email"].as_str().unwrap_or("").to_owned(), v["nonce"].as_str().unwrap_or("").to_owned()))
}

/// Seconds since 1970.
pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Held while the saved login is read, refreshed and written, so two
/// requests never refresh the same rotating token at once.
pub static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// An agent for the sign-in server.
pub fn agent() -> ureq::Agent {
    super::client::agent(std::time::Duration::from_secs(30))
}

/// POSTs a form; the status and the body's text.
fn post_form(agent: &ureq::Agent, url: &str, body: &str) -> Result<(u16, String), AiError> {
    let mut r = agent
        .post(url)
        .header("content-type", "application/x-www-form-urlencoded")
        .send(body)
        .map_err(|e| AiError::Network(e.to_string()))?;
    let status = r.status().as_u16();
    let text = r.body_mut().with_config().limit(1024 * 1024).read_to_string().map_err(|e| AiError::Network(e.to_string()))?;
    Ok((status, text))
}

/// The OAuth error in a failed token response, e.g. "invalid_grant".
fn oauth_code(body: &str) -> String {
    serde_json::from_str::<Value>(body).ok().and_then(|v| v["error"].as_str().map(str::to_owned)).unwrap_or_default()
}

fn oauth_error(status: u16, body: &str) -> AiError {
    match oauth_code(body) {
        code if code.is_empty() => super::wire::error_for(status, body),
        code => AiError::Login(format!("sign-in failed: {code}")),
    }
}

/// Trades the code from the browser for tokens.
pub fn exchange(agent: &ureq::Agent, auth: &str, client_id: &str, code: &str, p: &Pending) -> Result<Tokens, AiError> {
    let body = form(&[
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code", code),
        ("code_verifier", &p.verifier),
        ("redirect_uri", &p.redirect),
        ("resource", API),
    ]);
    let (status, text) = post_form(agent, &format!("{auth}/api/accounts/oauth/token"), &body)?;
    if (200..300).contains(&status) { parse_tokens(&text, now()) } else { Err(oauth_error(status, &text)) }
}

/// Gets a new access token (and a new refresh token, which replaces the
/// old). A refresh token OpenAI no longer accepts signs out.
pub fn refresh(agent: &ureq::Agent, auth: &str, login: &mut Login, now: u64) -> Result<(), AiError> {
    let body = form(&[
        ("grant_type", "refresh_token"),
        ("client_id", &login.client_id),
        ("refresh_token", &login.refresh_token),
        ("resource", API),
    ]);
    let (status, text) = post_form(agent, &format!("{auth}/api/accounts/oauth/token"), &body)?;
    if (200..300).contains(&status) {
        let tokens = parse_tokens_with(&text, now, &login.scopes)?;
        login.take_tokens(tokens);
        return Ok(());
    }
    let code = oauth_code(&text);
    let dead = ["invalid_grant", "invalid_refresh_token", "token_expired", "refresh_token_expired", "refresh_token_invalidated", "refresh_token_reused"];
    if dead.contains(&code.as_str()) {
        log::info!("ChatGPT sign-in ended: {code}");
        login.sign_out();
        return Err(AiError::SignInAgain);
    }
    Err(oauth_error(status, &text))
}

/// Tells OpenAI to forget the login's refresh token (sign-out). Failures
/// are only logged: the tokens are cleared locally either way.
pub fn revoke(agent: &ureq::Agent, auth: &str, login: &Login) {
    if login.refresh_token.is_empty() {
        return;
    }
    let endpoint = agent
        .get(&format!("{auth}/.well-known/openid-configuration"))
        .call()
        .ok()
        .and_then(|mut r| r.body_mut().read_to_string().ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["revocation_endpoint"].as_str().map(str::to_owned));
    let Some(endpoint) = endpoint else {
        log::warn!("ChatGPT sign-out: no revocation endpoint");
        return;
    };
    let body = form(&[("token", &login.refresh_token), ("token_type_hint", "refresh_token"), ("client_id", &login.client_id)]);
    match post_form(agent, &endpoint, &body) {
        Ok((200, _)) => log::info!("ChatGPT sign-in revoked"),
        Ok((status, _)) => log::warn!("ChatGPT sign-out: revocation answered {status}"),
        Err(e) => log::warn!("ChatGPT sign-out: {e}"),
    }
}

/// The page the browser shows when it comes back. Error text can carry
/// what was in the address, so it is escaped.
fn callback_page(result: &Result<(String, String), String>) -> String {
    let text = match result {
        Ok(_) => "Signed in to ChatGPT. You can close this tab and go back to fastcnsl.".to_owned(),
        Err(e) => format!("fastcnsl: {e}"),
    };
    let text = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
    format!("<!doctype html><meta charset=utf-8><title>fastcnsl</title><p style=\"font:16px sans-serif\">{text}</p>")
}

/// How long the browser has to come back.
const SIGN_IN_TIME: std::time::Duration = std::time::Duration::from_secs(300);

/// Signs in: opens the sign-in page with `open`, waits on `listener` for
/// the browser to come back (until `cancel` or five minutes), trades the
/// code for tokens and saves the login at `path`. The signed-in email.
pub fn sign_in_with(
    listener: std::net::TcpListener,
    auth: &str,
    path: &Path,
    open: impl FnOnce(&str),
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<String, AiError> {
    use std::io::{Read, Write};
    use std::sync::atomic::Ordering;

    let mut login = {
        let _one = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        Login::load(path)
    };
    if login.host_id.is_empty() {
        login.host_id = new_host_id();
    }
    let port = listener.local_addr().map_err(|e| AiError::Login(e.to_string()))?.port();
    let pending = Pending::new(format!("http://127.0.0.1:{port}{CALLBACK_PATH}"));
    listener.set_nonblocking(true).map_err(|e| AiError::Login(e.to_string()))?;
    open(&authorize_url(auth, &login, &pending));

    let deadline = std::time::Instant::now() + SIGN_IN_TIME;
    let (code, client_id) = loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(AiError::Cancelled);
        }
        if std::time::Instant::now() > deadline {
            return Err(AiError::Login("the browser didn't come back in time".into()));
        }
        let mut stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
            Err(e) => return Err(AiError::Login(e.to_string())),
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        let mut buf = vec![0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let head = String::from_utf8_lossy(&buf[..n]);
        let target = head.lines().next().and_then(|l| l.split(' ').nth(1)).unwrap_or("");
        let result = parse_callback(target, &pending, &login.client_id);
        let status = if target.starts_with(CALLBACK_PATH) { "200 OK" } else { "404 Not Found" };
        let html = callback_page(&result);
        let _ = write!(stream, "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{html}", html.len());
        // A browser's other requests (the icon) don't end the sign-in.
        if !target.starts_with(CALLBACK_PATH) {
            continue;
        }
        break result.map_err(AiError::Login)?;
    };

    let tokens = exchange(&agent(), auth, &client_id, &code, &pending)?;
    let (email, nonce) = id_token_claims(&tokens.id_token).unwrap_or_default();
    if !tokens.id_token.is_empty() && nonce != pending.nonce {
        return Err(AiError::Login("the sign-in's ID token is not for this sign-in".into()));
    }
    let _one = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    login.client_id = client_id;
    login.email = email.clone();
    login.take_tokens(tokens);
    login.save(path).map_err(|e| AiError::Login(format!("could not save the sign-in: {e}")))?;
    Ok(email)
}

/// Signs in on a thread of its own, the browser coming back to port 1455
/// (another free port if it is taken); `AiEvent::SignedIn` when done.
pub fn sign_in(job: u64, proxy: winit::event_loop::EventLoopProxy<crate::session::UserEvent>, cancel: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    let _ = std::thread::Builder::new().name("ai-login".into()).spawn(move || {
        let result = (|| {
            let path = Login::path().ok_or_else(|| AiError::Login("no settings folder to keep the sign-in in".into()))?;
            let listener = std::net::TcpListener::bind("127.0.0.1:1455")
                .or_else(|_| std::net::TcpListener::bind("127.0.0.1:0"))
                .map_err(|e| AiError::Login(format!("could not listen for the browser: {e}")))?;
            let open = |url: &str| {
                if let Err(e) = crate::open::open(url) {
                    log::warn!("could not open the browser: {e}");
                }
            };
            sign_in_with(listener, &auth_base(), &path, open, &cancel)
        })();
        if result != Err(AiError::Cancelled) {
            let _ = proxy.send_event(crate::session::UserEvent::Ai(super::AiEvent::SignedIn { job, result }));
        }
    });
}

#[cfg(test)]
mod tests {
    use base64::Engine;

    use super::*;

    fn pending() -> Pending {
        Pending { state: "st".into(), nonce: "no".into(), verifier: "v".repeat(43), redirect: "http://127.0.0.1:1455/auth/callback".into() }
    }

    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    use crate::ai::client::Client;
    use crate::ai::client::testserver::serve_with;
    use crate::ai::wire::{Request, user_message};

    /// The value of `name` in `url`'s query.
    fn query(url: &str, name: &str) -> String {
        url.split_once('?').unwrap().1.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| decode(v)).unwrap()
    }

    fn id_token(email: &str, nonce: &str) -> String {
        let p = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!(r#"{{"email":"{email}","nonce":"{nonce}"}}"#));
        format!("h.{p}.s")
    }

    fn tokens(access: &str, refresh: &str, id: &str) -> String {
        format!(r#"{{"access_token":"{access}","refresh_token":"{refresh}","id_token":"{id}","expires_in":3600,"scope":"openid email offline_access resource.invoke chatgpt.tokens.use.direct"}}"#)
    }

    const COMPLETED: &str = "data: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"ok\"}]}]}}\n\n";

    #[test]
    fn sign_in_with_a_fake_openai() {
        let dir = crate::testterm::temp_dir("signin");
        let path = dir.join("chatgpt-login.json");
        let nonce = Arc::new(Mutex::new(String::new()));
        let n = nonce.clone();
        let (auth, got) = serve_with(1, move |_| (200, tokens("a1", "r1", &id_token("me@x.org", &n.lock().unwrap()))));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let open = |url: &str| {
            assert!(url.contains("client_id=dynamic_agent_client"));
            *nonce.lock().unwrap() = query(url, "nonce");
            let back = format!("{}?code=c9&state={}&client_id=oaiapp_1", query(url, "redirect_uri"), query(url, "state"));
            std::thread::spawn(move || ureq::get(&back).call().map(|_| ()));
        };
        let email = sign_in_with(listener, &auth, &path, open, &AtomicBool::new(false)).unwrap();
        assert_eq!(email, "me@x.org");
        let l = Login::load(&path);
        assert_eq!((l.client_id.as_str(), l.access_token.as_str(), l.refresh_token.as_str()), ("oaiapp_1", "a1", "r1"));
        assert!(l.host_id.starts_with("urn:uuid:") && l.signed_in());
        let req = &got.join().unwrap()[0];
        assert!(req.starts_with("POST /api/accounts/oauth/token "), "{req}");
        for part in ["grant_type=authorization_code", "client_id=oaiapp_1", "code=c9", "code_verifier=", "resource=https%3A%2F%2Fapi.openai.com%2Fv1"] {
            assert!(req.contains(part), "{part} in {req}");
        }
    }

    #[test]
    fn a_cancelled_sign_in_stops() {
        let dir = crate::testterm::temp_dir("signin-cancel");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let started = std::time::Instant::now();
        let r = sign_in_with(listener, "http://127.0.0.1:9", &dir.join("l.json"), |_| {}, &AtomicBool::new(true));
        assert_eq!(r.unwrap_err(), AiError::Cancelled);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    fn signed_in(path: &Path, expires_at: u64) {
        let l = Login { client_id: "oaiapp_1".into(), host_id: "h".into(), email: "e".into(), access_token: "old".into(), refresh_token: "r1".into(), expires_at, scopes: vec![PLAN_SCOPE.into()], ..Login::default() };
        l.save(path).unwrap();
    }

    fn ask(c: &Client) -> Result<String, AiError> {
        let m = [user_message("u")];
        c.send(&Request { model: "m", system: "s", messages: &m, tools: &[], max_tokens: 10 }).map(|r| r.text)
    }

    #[test]
    fn an_expired_token_is_refreshed_first() {
        let dir = crate::testterm::temp_dir("refresh");
        let path = dir.join("chatgpt-login.json");
        signed_in(&path, 0);
        let (auth, auth_got) = serve_with(1, |_| (200, tokens("new", "r2", "")));
        let (api, api_got) = serve_with(1, |_| (200, COMPLETED.to_owned()));
        let c = Client::chatgpt_with(path.clone(), &auth, &api);
        assert_eq!(ask(&c).unwrap(), "ok");
        let refresh = &auth_got.join().unwrap()[0];
        assert!(refresh.contains("grant_type=refresh_token") && refresh.contains("refresh_token=r1") && refresh.contains("client_id=oaiapp_1"), "{refresh}");
        let call = &api_got.join().unwrap()[0];
        assert!(call.starts_with("POST /responses ") && call.to_lowercase().contains("authorization: bearer new"), "{call}");
        assert!(call.contains(r#""store":false"#));
        let l = Login::load(&path);
        assert_eq!((l.access_token.as_str(), l.refresh_token.as_str()), ("new", "r2"));
    }

    #[test]
    fn a_dead_refresh_token_signs_out() {
        let dir = crate::testterm::temp_dir("refresh-dead");
        let path = dir.join("chatgpt-login.json");
        signed_in(&path, 0);
        let (auth, _) = serve_with(1, |_| (400, r#"{"error":"invalid_grant"}"#.to_owned()));
        let c = Client::chatgpt_with(path.clone(), &auth, "http://127.0.0.1:9");
        assert_eq!(ask(&c).unwrap_err(), AiError::SignInAgain);
        let l = Login::load(&path);
        assert!(!l.signed_in());
        assert_eq!(l.client_id, "oaiapp_1");
    }

    #[test]
    fn a_fresh_token_is_used_as_is() {
        let dir = crate::testterm::temp_dir("fresh");
        let path = dir.join("chatgpt-login.json");
        signed_in(&path, now() + 3600);
        let (api, api_got) = serve_with(1, |_| (200, COMPLETED.to_owned()));
        let c = Client::chatgpt_with(path, "http://127.0.0.1:9", &api);
        assert_eq!(ask(&c).unwrap(), "ok");
        assert!(api_got.join().unwrap()[0].to_lowercase().contains("authorization: bearer old"));
    }

    #[test]
    fn sign_out_revokes_the_refresh_token() {
        let base = Arc::new(Mutex::new(String::new()));
        let b = base.clone();
        let (auth, got) = serve_with(2, move |raw| {
            if raw.starts_with("GET /.well-known/openid-configuration") {
                (200, format!(r#"{{"revocation_endpoint":"{}/oauth/revoke"}}"#, b.lock().unwrap()))
            } else {
                (200, String::new())
            }
        });
        *base.lock().unwrap() = auth.clone();
        let l = Login { client_id: "oaiapp_1".into(), refresh_token: "r1".into(), ..Login::default() };
        revoke(&agent(), &auth, &l);
        let reqs = got.join().unwrap();
        let post = &reqs[1];
        assert!(post.starts_with("POST /oauth/revoke "), "{post}");
        for part in ["token=r1", "token_type_hint=refresh_token", "client_id=oaiapp_1"] {
            assert!(post.contains(part), "{part} in {post}");
        }
    }

    #[test]
    fn the_browser_page_escapes_what_came_back() {
        let page = callback_page(&Err("sign-in failed: <script>x</script>&".into()));
        assert!(page.contains("&lt;script&gt;x&lt;/script&gt;&amp;") && !page.contains("<script>"), "{page}");
        assert!(callback_page(&Ok(("c".into(), "i".into()))).contains("You can close this tab"));
    }

    #[test]
    fn pkce_challenge_is_base64url_sha256() {
        // Checked with Python: base64.urlsafe_b64encode(hashlib.sha256(v).digest()) without "=".
        assert_eq!(challenge("dBjftJeZ4CVP-mJ92K9pTAgf-ZZqJNx4Xx1pTRxv8GM"), "axKhVlajcpYVk-9YtwWg6UWZiJ5koaTRDgsCjRMgGuA");
        let t = random_token(32);
        assert_eq!(t.len(), 43);
        assert_ne!(t, random_token(32));
    }

    #[test]
    fn first_authorize_url_registers_fastcnsl() {
        let login = Login { host_id: "urn:uuid:1".into(), ..Login::default() };
        let u = authorize_url(AUTH, &login, &pending());
        assert!(u.starts_with("https://auth.openai.com/api/accounts/authorize?"));
        for part in [
            "client_id=dynamic_agent_client",
            "agent_name_hint=fastcnsl",
            "ext_agent_host_id=urn%3Auuid%3A1",
            "response_type=code",
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback",
            "scope=openid%20profile%20email%20offline_access%20resource.invoke%20chatgpt.tokens.use.direct",
            "resource=https%3A%2F%2Fapi.openai.com%2Fv1",
            "state=st",
            "nonce=no",
            "code_challenge_method=S256",
        ] {
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
        assert!(parse_callback("/auth/callback?code=c&state=st", &p, "").is_err(), "no client id at all");
        assert!(parse_callback("/favicon.ico", &p, "").is_err());
        assert_eq!(parse_callback("/auth/callback?code=a%2Fb&state=st&client_id=x", &p, "").unwrap().0, "a/b");
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
        let mut l = Login {
            client_id: "c".into(),
            host_id: "h".into(),
            email: "e".into(),
            refresh_token: "r".into(),
            access_token: "a".into(),
            expires_at: 100,
            scopes: vec!["chatgpt.tokens.use.direct".into()],
            welcomed: true,
            ..Login::default()
        };
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
}
