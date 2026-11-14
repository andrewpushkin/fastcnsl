//! OpenRouter: one key for many providers' models, some of them free.
//! Requests use the Chat Completions format at `{SITE}/api/v1`.
//!
//! "Sign in with OpenRouter" is OpenRouter's own way for apps to get a key
//! (OAuth PKCE, the browser coming back to this computer): the user signs
//! in on openrouter.ai, and the key it makes for fastcnsl is kept in
//! `ai-keys.toml` like a pasted one. The key can be seen and deleted at
//! openrouter.ai/settings/keys.

use std::path::Path;

use serde_json::{Value, json};

use super::AiError;
use super::login::{self, decode};

/// The site: sign-in at `/auth`, the API at `/api/v1`.
pub const SITE: &str = "https://openrouter.ai";
/// Picks a free model that can do what the request needs (tools here).
pub const FREE_ROUTER: &str = "openrouter/free";
/// Where the browser comes back to (the port is any free one).
const CALLBACK_PATH: &str = "/callback";
/// The label of the key OpenRouter makes, on its keys page.
const KEY_LABEL: &str = "fastcnsl";

/// The headers that name fastcnsl to OpenRouter (its app pages and logs).
pub const ATTRIBUTION: [(&str, &str); 3] = [
    ("http-referer", env!("CARGO_PKG_REPOSITORY")),
    ("x-openrouter-title", "fastcnsl"),
    ("x-openrouter-categories", "cli-agent"),
];

/// The site, or in debug builds `FASTCNSL_OPENROUTER` (tests against a
/// fake server).
pub fn site() -> String {
    let over = if cfg!(debug_assertions) { std::env::var("FASTCNSL_OPENROUTER").ok().filter(|v| !v.is_empty()) } else { None };
    over.unwrap_or_else(|| SITE.to_owned()).trim_end_matches('/').to_owned()
}

/// The API under `site`.
pub fn api_base(site: &str) -> String {
    format!("{site}/api/v1")
}

/// The models to offer, from the ids the API lists: the free router first,
/// then the free models, then the rest, each part sorted.
pub fn order_models(mut ids: Vec<String>) -> Vec<String> {
    ids.retain(|m| m != FREE_ROUTER);
    ids.sort_by_key(|m| (!m.ends_with(":free"), m.clone()));
    ids.dedup();
    std::iter::once(FREE_ROUTER.to_owned()).chain(ids).collect()
}

/// The reason in an error reply, e.g. the free models' daily limit
/// ("Rate limit exceeded: free-models-per-day. Add 10 credits…").
pub fn error_message(body: &str) -> Option<String> {
    let v = serde_json::from_str::<Value>(body).ok()?;
    let m = v["error"]["message"].as_str()?.trim();
    (!m.is_empty()).then(|| m.to_owned())
}

/// The address the browser opens to sign in.
pub fn authorize_url(site: &str, callback: &str, verifier: &str, state: &str) -> String {
    let q = login::form(&[
        ("callback_url", callback),
        ("code_challenge", &login::challenge(verifier)),
        ("code_challenge_method", "S256"),
        ("state", state),
        ("key_label", KEY_LABEL),
    ]);
    format!("{site}/auth?{q}")
}

/// Checks the browser's return to `target` (path and query): the code.
pub fn parse_callback(target: &str, state: &str) -> Result<String, String> {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != CALLBACK_PATH {
        return Err("not the sign-in callback".into());
    }
    let get = |name: &str| query.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| decode(v));
    if let Some(e) = get("error") {
        return Err(match get("error_description").filter(|d| !d.is_empty()) {
            Some(d) => format!("OpenRouter didn't allow the sign-in: {d}"),
            None if e == "access_denied" => "sign-in was cancelled".into(),
            None => format!("sign-in failed: {e}"),
        });
    }
    if get("state").as_deref() != Some(state) {
        return Err("the sign-in came back with the wrong state".into());
    }
    get("code").filter(|c| !c.is_empty()).ok_or_else(|| "the sign-in brought no code".into())
}

/// Trades the code from the browser for a key.
pub fn exchange(agent: &ureq::Agent, site: &str, code: &str, verifier: &str) -> Result<String, AiError> {
    let body = json!({"code": code, "code_verifier": verifier, "code_challenge_method": "S256"}).to_string();
    let mut r = agent
        .post(&format!("{}/auth/keys", api_base(site)))
        .header("content-type", "application/json")
        .send(&body)
        .map_err(|e| AiError::Network(e.to_string()))?;
    let status = r.status().as_u16();
    let text = r.body_mut().with_config().limit(1024 * 1024).read_to_string().map_err(|e| AiError::Network(e.to_string()))?;
    if !(200..300).contains(&status) {
        // An error body has no secrets: log it, for diagnosing.
        log::warn!("OpenRouter key exchange: {status}: {}", text.chars().take(500).collect::<String>());
        let why = error_message(&text).unwrap_or_else(|| format!("status {status}"));
        return Err(AiError::Login(format!("OpenRouter didn't give a key: {why}")));
    }
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v["key"].as_str().map(str::to_owned))
        .filter(|k| !k.is_empty())
        .ok_or_else(|| AiError::Login("OpenRouter's answer had no key".into()))
}

/// Signs in: opens the sign-in page with `open`, waits on `listener` for
/// the browser to come back (until `cancel` or five minutes), trades the
/// code for a key and saves it in the keys file at `keys`.
pub fn sign_in_with(
    listener: std::net::TcpListener,
    site: &str,
    keys: &Path,
    open: impl FnOnce(&str),
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<(), AiError> {
    let port = listener.local_addr().map_err(|e| AiError::Login(e.to_string()))?.port();
    listener.set_nonblocking(true).map_err(|e| AiError::Login(e.to_string()))?;
    let deadline = std::time::Instant::now() + login::SIGN_IN_TIME;
    let callback = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");
    let verifier = login::random_token(48);
    let state = login::random_token(24);
    open(&authorize_url(site, &callback, &verifier, &state));

    let check = |target: &str| parse_callback(target, &state);
    let (mut browser, outcome) = login::wait_for_browser(&listener, CALLBACK_PATH, check, "OpenRouter", cancel, deadline)?;
    let result = outcome.map_err(AiError::Login).and_then(|code| exchange(&login::agent(), site, &code, &verifier)).and_then(|key| {
        let mut k = super::Keys::load(keys);
        k.set(super::Provider::OpenRouter, Some(key));
        k.save(keys).map_err(|e| AiError::Login(format!("could not save the key: {e}")))
    });
    let page = match &result {
        Ok(()) => "Signed in to OpenRouter. You can close this tab and go back to fastcnsl.".to_owned(),
        Err(e) => format!("fastcnsl: {e}"),
    };
    login::answer_browser(&mut browser, "200 OK", &page);
    result
}

/// Signs in on a thread of its own; `AiEvent::SignedIn` when done (with
/// no email: OpenRouter gives only the key).
pub fn sign_in(job: u64, proxy: winit::event_loop::EventLoopProxy<crate::session::UserEvent>, cancel: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    let _ = std::thread::Builder::new().name("ai-login".into()).spawn(move || {
        let result = (|| {
            let keys = super::Keys::path().ok_or_else(|| AiError::Login("no settings folder to keep the key in".into()))?;
            let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| AiError::Login(format!("could not listen for the browser: {e}")))?;
            let open = |url: &str| {
                if let Err(e) = crate::open::open(url) {
                    log::warn!("could not open the browser: {e}");
                }
            };
            sign_in_with(listener, &site(), &keys, open, &cancel)
        })();
        if result != Err(AiError::Cancelled) {
            let result = result.map(|()| String::new());
            let _ = proxy.send_event(crate::session::UserEvent::Ai(super::AiEvent::SignedIn { job, provider: super::Provider::OpenRouter, result }));
        }
    });
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::ai::client::testserver::serve_with;
    use crate::ai::login::encode;

    #[test]
    fn free_models_come_first() {
        let ids = ["z/paid", "a/paid", "m/x:free", "openrouter/free", "b/y:free"].map(String::from).to_vec();
        assert_eq!(order_models(ids), ["openrouter/free", "b/y:free", "m/x:free", "a/paid", "z/paid"]);
        // The router is offered even when the list leaves it out.
        assert_eq!(order_models(vec!["a/paid".into()]), ["openrouter/free", "a/paid"]);
    }

    #[test]
    fn authorize_url_carries_the_challenge_and_callback() {
        let url = authorize_url("https://openrouter.ai", "http://127.0.0.1:5/callback", "verifier", "st");
        assert!(url.starts_with("https://openrouter.ai/auth?callback_url=http%3A%2F%2F127.0.0.1%3A5%2Fcallback&"), "{url}");
        assert!(url.contains(&format!("code_challenge={}", login::challenge("verifier"))));
        assert!(url.contains("code_challenge_method=S256") && url.contains("state=st") && url.contains("key_label=fastcnsl"));
    }

    #[test]
    fn callback_needs_the_state_and_a_code() {
        assert_eq!(parse_callback("/callback?code=abc&state=st", "st"), Ok("abc".into()));
        assert!(parse_callback("/callback?code=abc&state=other", "st").unwrap_err().contains("wrong state"));
        assert!(parse_callback("/callback?state=st", "st").unwrap_err().contains("no code"));
        assert_eq!(parse_callback("/callback?error=access_denied", "st"), Err("sign-in was cancelled".into()));
        assert!(parse_callback("/elsewhere?code=abc&state=st", "st").is_err());
    }

    #[test]
    fn exchange_sends_the_verifier_and_reads_the_key() {
        let (base, server) = serve_with(1, |req| {
            assert!(req.starts_with("POST /api/v1/auth/keys "), "{req}");
            let body: Value = serde_json::from_str(req.split("\r\n\r\n").nth(1).unwrap()).unwrap();
            assert_eq!((body["code"].as_str(), body["code_verifier"].as_str(), body["code_challenge_method"].as_str()), (Some("c1"), Some("v1"), Some("S256")));
            (200, r#"{"key":"sk-or-v1-abc","user_id":"u"}"#.into())
        });
        assert_eq!(exchange(&login::agent(), &base, "c1", "v1").unwrap(), "sk-or-v1-abc");
        server.join().unwrap();

        let (base, server) = serve_with(1, |_| (403, r#"{"error":{"code":403,"message":"Invalid code or code_verifier"}}"#.into()));
        let e = exchange(&login::agent(), &base, "c1", "v1").unwrap_err();
        assert_eq!(e, AiError::Login("OpenRouter didn't give a key: Invalid code or code_verifier".into()));
        server.join().unwrap();
    }

    #[test]
    fn error_message_reads_the_reason() {
        let body = r#"{"error":{"message":"Rate limit exceeded: free-models-per-day","code":429}}"#;
        assert_eq!(error_message(body).as_deref(), Some("Rate limit exceeded: free-models-per-day"));
        assert_eq!(error_message("not json"), None);
    }

    /// The whole sign-in against a fake OpenRouter: the "browser" follows
    /// the page's callback with the code, and the key lands in the file.
    #[test]
    fn sign_in_saves_the_key() {
        let (site, server) = serve_with(1, |_| (200, r#"{"key":"sk-or-v1-new"}"#.into()));
        let dir = crate::testterm::temp_dir("openrouter-signin");
        let keys = dir.join("ai-keys.toml");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let browser = std::sync::Mutex::new(None);
        let open = |url: &str| {
            let q = url.split_once('?').unwrap().1;
            let get = |n: &str| q.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == n).map(|(_, v)| decode(v)).unwrap();
            let callback = get("callback_url");
            let state = get("state");
            *browser.lock().unwrap() = Some(std::thread::spawn(move || {
                let addr = callback.trim_start_matches("http://").split('/').next().unwrap().to_owned();
                let mut s = std::net::TcpStream::connect(addr).unwrap();
                write!(s, "GET /callback?code=c9&state={} HTTP/1.1\r\nhost: x\r\n\r\n", encode(&state)).unwrap();
                let mut page = String::new();
                let _ = s.read_to_string(&mut page);
                page
            }));
        };
        sign_in_with(listener, &site, &keys, open, &AtomicBool::new(false)).unwrap();
        let page = browser.lock().unwrap().take().unwrap().join().unwrap();
        assert!(page.contains("Signed in to OpenRouter"), "{page}");
        assert_eq!(crate::ai::Keys::load(&keys).in_file(crate::ai::Provider::OpenRouter), Some("sk-or-v1-new"));
        server.join().unwrap();
    }
}
