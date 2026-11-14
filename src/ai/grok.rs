//! "Sign in with Grok": a SuperGrok or X Premium plan instead of an API key.
//! xAI has no registration for apps: every app signing in with a plan uses
//! xAI's shared client (the Grok Build CLI's id) and names itself with
//! `referrer`, so xAI's page says "Grok Build". The sign-in is a device
//! code (RFC 8628): fastcnsl shows a code, opens xAI's page with it filled
//! in, and asks until the user approves. The tokens are kept in
//! `grok-login.json` beside the settings file, readable by the user only.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::AiError;
use super::login::{form, oauth_code, oauth_error, post_form};
use crate::config::Config;

/// xAI's sign-in server.
pub const AUTH: &str = "https://auth.x.ai";
/// xAI's API, where a plan's requests go.
pub const API: &str = "https://api.x.ai/v1";
/// xAI's shared client for apps that sign in with a plan.
pub const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
/// Who the user is, staying signed in, and spending the plan.
const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
/// How fastcnsl names itself to xAI.
const REFERRER: &str = "fastcnsl";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// A token this close to expiring (seconds) is refreshed before a request.
const REFRESH_EARLY: u64 = 300;

/// The sign-in server, or in debug builds `FASTCNSL_GROK_AUTH`.
pub fn auth_base() -> String {
    debug_override("FASTCNSL_GROK_AUTH").unwrap_or_else(|| AUTH.to_owned())
}

/// The API, or in debug builds `FASTCNSL_GROK_API`.
pub fn api_base() -> String {
    debug_override("FASTCNSL_GROK_API").unwrap_or_else(|| API.to_owned())
}

fn debug_override(var: &str) -> Option<String> {
    if cfg!(debug_assertions) { std::env::var(var).ok().filter(|v| !v.is_empty()) } else { None }
}

/// What is kept between runs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GrokLogin {
    pub email: String,
    pub access_token: String,
    pub refresh_token: String,
    /// When the access token expires, in seconds since 1970.
    pub expires_at: u64,
}

impl GrokLogin {
    /// `grok-login.json` beside the settings file.
    pub fn path() -> Option<PathBuf> {
        Some(Config::path()?.with_file_name("grok-login.json"))
    }

    /// Reads `path`; a missing or broken file is a login with nothing in it.
    pub fn load(path: &Path) -> GrokLogin {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|_| {
                log::warn!("{} is not valid: signed out of Grok", path.display());
                GrokLogin::default()
            }),
            Err(_) => GrokLogin::default(),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        super::login::save_json(self, path)
    }

    pub fn signed_in(&self) -> bool {
        !self.refresh_token.is_empty()
    }

    /// The access token expires within five minutes of `now`.
    pub fn needs_refresh(&self, now: u64) -> bool {
        now + REFRESH_EARLY >= self.expires_at
    }

    fn take_tokens(&mut self, t: GrokTokens) {
        self.access_token = t.access_token;
        if !t.refresh_token.is_empty() {
            self.refresh_token = t.refresh_token;
        }
        self.expires_at = t.expires_at;
    }

    pub fn sign_out(&mut self) {
        *self = GrokLogin::default();
    }
}

/// A device code to show the user.
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub device_code: String,
    /// What the user types on xAI's page, e.g. "ABCD-1234".
    pub user_code: String,
    /// xAI's page.
    pub page: String,
    /// The page with the code filled in.
    pub page_with_code: String,
    /// Seconds the code is good for.
    pub expires_in: u64,
    /// Seconds between asking whether the user approved.
    pub interval: u64,
}

/// Asks xAI for a device code.
pub fn request_device(agent: &ureq::Agent, auth: &str) -> Result<Device, AiError> {
    let body = form(&[("client_id", CLIENT_ID), ("scope", SCOPE), ("referrer", REFERRER)]);
    let (status, text) = post_form(agent, &format!("{auth}/oauth2/device/code"), &body)?;
    if !(200..300).contains(&status) {
        log::warn!("Grok sign-in: device code: {status}: {}", text.chars().take(1000).collect::<String>());
        return Err(oauth_error(status, &text));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| AiError::BadReply(format!("device code: {e}")))?;
    let s = |k: &str| v[k].as_str().unwrap_or("").to_owned();
    let (device_code, user_code, page) = (s("device_code"), s("user_code"), s("verification_uri"));
    if device_code.is_empty() || user_code.is_empty() || page.is_empty() {
        return Err(AiError::BadReply("no device code".into()));
    }
    let page_with_code = Some(s("verification_uri_complete")).filter(|p| !p.is_empty()).unwrap_or_else(|| page.clone());
    Ok(Device { device_code, user_code, page, page_with_code, expires_in: v["expires_in"].as_u64().unwrap_or(900), interval: v["interval"].as_u64().unwrap_or(5) })
}

/// One answer to "did the user approve yet?".
#[derive(Debug, Clone, PartialEq)]
pub enum Poll {
    Pending,
    /// Ask less often.
    SlowDown,
    Done(GrokTokens),
}

/// Tokens from the token endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct GrokTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub expires_at: u64,
}

/// Asks once whether the user approved `device_code`.
pub fn poll(agent: &ureq::Agent, auth: &str, device_code: &str) -> Result<Poll, AiError> {
    let body = form(&[("grant_type", DEVICE_GRANT), ("client_id", CLIENT_ID), ("device_code", device_code)]);
    let (status, text) = post_form(agent, &format!("{auth}/oauth2/token"), &body)?;
    if (200..300).contains(&status) {
        return parse_tokens(&text, super::login::now()).map(Poll::Done);
    }
    match oauth_code(&text).as_str() {
        "authorization_pending" => Ok(Poll::Pending),
        "slow_down" => Ok(Poll::SlowDown),
        "access_denied" | "authorization_denied" => Err(AiError::Login("sign-in was cancelled".into())),
        "expired_token" => Err(AiError::Login(EXPIRED.into())),
        _ => {
            log::warn!("Grok sign-in: token: {status}: {}", text.chars().take(1000).collect::<String>());
            Err(if status == 403 { AiError::GrokPlan } else { oauth_error(status, &text) })
        }
    }
}

const EXPIRED: &str = "the code expired; sign in again";

/// Reads a token response; `now` in seconds since 1970.
pub fn parse_tokens(body: &str, now: u64) -> Result<GrokTokens, AiError> {
    let v: Value = serde_json::from_str(body).map_err(|e| AiError::BadReply(format!("tokens: {e}")))?;
    let s = |k: &str| v[k].as_str().unwrap_or("").to_owned();
    let access_token = s("access_token");
    if access_token.is_empty() {
        return Err(AiError::BadReply("no access token".into()));
    }
    let expires_at = match v["expires_in"].as_u64() {
        Some(secs) => now + secs,
        None => token_expiry(&access_token).unwrap_or(now + 3600),
    };
    Ok(GrokTokens { access_token, refresh_token: s("refresh_token"), id_token: s("id_token"), expires_at })
}

/// `exp` from a JWT's payload (the access token, when xAI sends no
/// `expires_in`).
fn token_expiry(jwt: &str) -> Option<u64> {
    let payload = jwt.split('.').nth(1)?;
    let v: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?).ok()?;
    v["exp"].as_u64()
}

/// The signed-in email: from the ID token, else from xAI's userinfo; ""
/// when neither says.
pub fn email_of(agent: &ureq::Agent, auth: &str, tokens: &GrokTokens) -> String {
    if let Some((email, _)) = super::login::id_token_claims(&tokens.id_token).filter(|(e, _)| !e.is_empty()) {
        return email;
    }
    agent
        .get(&format!("{auth}/oauth2/userinfo"))
        .header("authorization", &format!("Bearer {}", tokens.access_token))
        .call()
        .ok()
        .filter(|r| r.status().is_success())
        .and_then(|mut r| r.body_mut().read_to_string().ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["email"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Gets a new access token. A refresh token xAI no longer takes signs out;
/// a 403 is the plan gate and keeps the login.
pub fn refresh(agent: &ureq::Agent, auth: &str, login: &mut GrokLogin, now: u64) -> Result<(), AiError> {
    let body = form(&[("grant_type", "refresh_token"), ("client_id", CLIENT_ID), ("refresh_token", &login.refresh_token)]);
    let (status, text) = post_form(agent, &format!("{auth}/oauth2/token"), &body)?;
    match status {
        200..=299 => {
            login.take_tokens(parse_tokens(&text, now)?);
            Ok(())
        }
        403 => Err(AiError::GrokPlan),
        400 | 401 => {
            log::info!("Grok sign-in ended: {}", oauth_code(&text));
            login.sign_out();
            Err(AiError::SignInAgain)
        }
        _ => Err(oauth_error(status, &text)),
    }
}

/// Tells xAI to forget the login's refresh token (sign-out). Failures are
/// only logged: the tokens are cleared locally either way.
pub fn revoke(agent: &ureq::Agent, auth: &str, login: &GrokLogin) {
    if login.refresh_token.is_empty() {
        return;
    }
    let body = form(&[("token", &login.refresh_token), ("token_type_hint", "refresh_token"), ("client_id", CLIENT_ID)]);
    match post_form(agent, &format!("{auth}/oauth2/revoke"), &body) {
        Ok((200, _)) => log::info!("Grok sign-in revoked"),
        Ok((status, _)) => log::warn!("Grok sign-out: revocation answered {status}"),
        Err(e) => log::warn!("Grok sign-out: {e}"),
    }
}

/// Signs in: asks for a device code, shows it with `shown` and opens xAI's
/// page with `open`, then asks every `interval` seconds until the user
/// approves (or `cancel`, or the code expires), and saves the login at
/// `path`. The signed-in email.
pub fn sign_in_with(
    auth: &str,
    path: &Path,
    shown: impl FnOnce(&Device),
    open: impl FnOnce(&str),
    cancel: &AtomicBool,
) -> Result<String, AiError> {
    let agent = super::login::agent();
    let device = request_device(&agent, auth)?;
    shown(&device);
    open(&device.page_with_code);
    let deadline = Instant::now() + Duration::from_secs(device.expires_in);
    let mut interval = device.interval;
    let tokens = loop {
        // Waits before each ask, as RFC 8628 says, watching Cancel.
        let next = Instant::now() + Duration::from_secs(interval);
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(AiError::Cancelled);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(AiError::Login(EXPIRED.into()));
            }
            if now >= next {
                break;
            }
            std::thread::sleep(Duration::from_millis(100).min(next - now));
        }
        match poll(&agent, auth, &device.device_code)? {
            Poll::Pending => {}
            Poll::SlowDown => interval += 5,
            Poll::Done(t) => break t,
        }
    };
    log::info!("Grok sign-in: tokens received");
    let mut login = GrokLogin { email: email_of(&agent, auth, &tokens), ..GrokLogin::default() };
    login.take_tokens(tokens);
    let _one = super::login::LOCK.lock().unwrap_or_else(|e| e.into_inner());
    login.save(path).map_err(|e| AiError::Login(format!("could not save the sign-in: {e}")))?;
    Ok(login.email)
}

/// Signs in on a thread of its own: `AiEvent::DeviceCode` once the code is
/// known, `AiEvent::SignedIn` when done.
pub fn sign_in(job: u64, proxy: winit::event_loop::EventLoopProxy<crate::session::UserEvent>, cancel: std::sync::Arc<AtomicBool>) {
    use crate::session::UserEvent;
    use super::{AiEvent, Provider};
    let _ = std::thread::Builder::new().name("ai-login".into()).spawn(move || {
        let result = (|| {
            let path = GrokLogin::path().ok_or_else(|| AiError::Login("no settings folder to keep the sign-in in".into()))?;
            let shown = |d: &Device| {
                let _ = proxy.send_event(UserEvent::Ai(AiEvent::DeviceCode { job, code: d.user_code.clone(), page: d.page_with_code.clone() }));
            };
            let open = |url: &str| {
                if let Err(e) = crate::open::open(url) {
                    log::warn!("could not open the browser: {e}");
                }
            };
            sign_in_with(&auth_base(), &path, shown, open, &cancel)
        })();
        if result != Err(AiError::Cancelled) {
            let _ = proxy.send_event(UserEvent::Ai(AiEvent::SignedIn { job, provider: Provider::Xai, result }));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::client::testserver::serve_with;
    use crate::ai::login::decode;

    fn device_json(interval: u64) -> String {
        format!(
            r#"{{"device_code":"dc1","user_code":"ABCD-1234","verification_uri":"https://accounts.x.ai/oauth2/device","verification_uri_complete":"https://accounts.x.ai/oauth2/device?user_code=ABCD-1234","expires_in":1800,"interval":{interval}}}"#
        )
    }

    fn id_token(email: &str) -> String {
        format!("h.{}.s", URL_SAFE_NO_PAD.encode(format!(r#"{{"email":"{email}"}}"#)))
    }

    fn tokens(access: &str, refresh: &str, id: &str) -> String {
        format!(r#"{{"access_token":"{access}","refresh_token":"{refresh}","id_token":"{id}","expires_in":21600,"token_type":"Bearer"}}"#)
    }

    /// The form field `name` in a raw request's body.
    fn field(raw: &str, name: &str) -> Option<String> {
        raw.split("\r\n\r\n").nth(1)?.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| decode(v))
    }

    #[test]
    fn the_device_code_request_names_fastcnsl() {
        let (auth, got) = serve_with(1, |_| (200, device_json(5)));
        let d = request_device(&super::super::login::agent(), &auth).unwrap();
        assert_eq!(d.user_code, "ABCD-1234");
        assert_eq!(d.page, "https://accounts.x.ai/oauth2/device");
        assert_eq!(d.page_with_code, "https://accounts.x.ai/oauth2/device?user_code=ABCD-1234");
        assert_eq!((d.expires_in, d.interval), (1800, 5));
        let raw = &got.join().unwrap()[0];
        assert!(raw.starts_with("POST /oauth2/device/code "), "{raw}");
        assert_eq!(field(raw, "client_id").as_deref(), Some(CLIENT_ID));
        assert_eq!(field(raw, "scope").as_deref(), Some(SCOPE));
        assert_eq!(field(raw, "referrer").as_deref(), Some("fastcnsl"));
    }

    #[test]
    fn a_device_answer_without_the_filled_in_page_uses_the_plain_one() {
        let (auth, got) = serve_with(1, |_| (200, r#"{"device_code":"d","user_code":"U","verification_uri":"https://p","expires_in":60}"#.to_owned()));
        let d = request_device(&super::super::login::agent(), &auth).unwrap();
        assert_eq!((d.page_with_code.as_str(), d.interval), ("https://p", 5));
        got.join().unwrap();
    }

    #[test]
    fn polling_tells_pending_slow_down_and_tokens_apart() {
        let answers = [
            (400, r#"{"error":"authorization_pending","error_description":"User has not yet authorized"}"#.to_owned()),
            (400, r#"{"error":"slow_down"}"#.to_owned()),
            (200, tokens("a1", "r1", "")),
        ];
        let mut it = answers.into_iter();
        let (auth, got) = serve_with(3, move |_| it.next().unwrap());
        let agent = super::super::login::agent();
        assert_eq!(poll(&agent, &auth, "dc1").unwrap(), Poll::Pending);
        assert_eq!(poll(&agent, &auth, "dc1").unwrap(), Poll::SlowDown);
        let Poll::Done(t) = poll(&agent, &auth, "dc1").unwrap() else { panic!("no tokens") };
        assert_eq!((t.access_token.as_str(), t.refresh_token.as_str()), ("a1", "r1"));
        let raw = &got.join().unwrap()[2];
        assert!(raw.starts_with("POST /oauth2/token "), "{raw}");
        assert_eq!(field(raw, "grant_type").as_deref(), Some(DEVICE_GRANT));
        assert_eq!(field(raw, "device_code").as_deref(), Some("dc1"));
        assert_eq!(field(raw, "client_id").as_deref(), Some(CLIENT_ID));
    }

    #[test]
    fn declining_or_an_expired_code_is_explained() {
        let answers = [(400, r#"{"error":"access_denied"}"#), (400, r#"{"error":"expired_token"}"#)];
        let mut it = answers.into_iter();
        let (auth, got) = serve_with(2, move |_| {
            let (s, b) = it.next().unwrap();
            (s, b.to_owned())
        });
        let agent = super::super::login::agent();
        assert_eq!(poll(&agent, &auth, "d").unwrap_err(), AiError::Login("sign-in was cancelled".into()));
        assert_eq!(poll(&agent, &auth, "d").unwrap_err(), AiError::Login("the code expired; sign in again".into()));
        got.join().unwrap();
    }

    #[test]
    fn token_lifetime_comes_from_expires_in_else_the_token_itself() {
        assert_eq!(parse_tokens(&tokens("a", "r", ""), 1000).unwrap().expires_at, 1000 + 21600);
        let jwt = format!("h.{}.s", URL_SAFE_NO_PAD.encode(r#"{"exp":5000}"#));
        let body = format!(r#"{{"access_token":"{jwt}","refresh_token":"r"}}"#);
        assert_eq!(parse_tokens(&body, 1000).unwrap().expires_at, 5000);
        assert_eq!(parse_tokens(r#"{"access_token":"opaque"}"#, 1000).unwrap().expires_at, 1000 + 3600);
        assert!(matches!(parse_tokens(r#"{"refresh_token":"r"}"#, 0), Err(AiError::BadReply(_))));
    }

    #[test]
    fn sign_in_shows_the_code_opens_the_page_and_saves_the_login() {
        let dir = crate::testterm::temp_dir("grok-signin");
        let path = dir.join("grok-login.json");
        let answers = [
            (200, device_json(0)),
            (400, r#"{"error":"authorization_pending"}"#.to_owned()),
            (200, tokens("a1", "r1", &id_token("me@x.org"))),
        ];
        let mut it = answers.into_iter();
        let (auth, got) = serve_with(3, move |_| it.next().unwrap());
        let mut code = String::new();
        let mut opened = String::new();
        let email = sign_in_with(&auth, &path, |d| code = d.user_code.clone(), |url| opened = url.to_owned(), &AtomicBool::new(false)).unwrap();
        assert_eq!(email, "me@x.org");
        assert_eq!(code, "ABCD-1234");
        assert_eq!(opened, "https://accounts.x.ai/oauth2/device?user_code=ABCD-1234");
        let l = GrokLogin::load(&path);
        assert_eq!((l.email.as_str(), l.access_token.as_str(), l.refresh_token.as_str()), ("me@x.org", "a1", "r1"));
        assert!(l.signed_in());
        assert_eq!(got.join().unwrap().len(), 3);
    }

    #[test]
    fn without_an_id_token_the_email_comes_from_userinfo() {
        let dir = crate::testterm::temp_dir("grok-userinfo");
        let path = dir.join("grok-login.json");
        let (auth, got) = serve_with(3, |raw: &str| {
            if raw.starts_with("POST /oauth2/device/code") {
                (200, device_json(0))
            } else if raw.starts_with("POST /oauth2/token") {
                (200, tokens("a1", "r1", ""))
            } else {
                assert!(raw.to_lowercase().contains("authorization: bearer a1"), "{raw}");
                (200, r#"{"sub":"1","email":"u@x.org"}"#.to_owned())
            }
        });
        assert_eq!(sign_in_with(&auth, &path, |_| {}, |_| {}, &AtomicBool::new(false)).unwrap(), "u@x.org");
        assert!(got.join().unwrap()[2].starts_with("GET /oauth2/userinfo "));
    }

    #[test]
    fn cancel_stops_waiting_and_saves_nothing() {
        let dir = crate::testterm::temp_dir("grok-cancel");
        let path = dir.join("grok-login.json");
        let (auth, got) = serve_with(1, |_| (200, device_json(5)));
        let cancel = AtomicBool::new(false);
        let r = sign_in_with(&auth, &path, |_| cancel.store(true, Ordering::Relaxed), |_| {}, &cancel);
        assert_eq!(r.unwrap_err(), AiError::Cancelled);
        assert!(!path.exists());
        got.join().unwrap();
    }

    #[test]
    fn a_login_needs_refreshing_five_minutes_early() {
        let l = GrokLogin { refresh_token: "r".into(), expires_at: 10_000, ..GrokLogin::default() };
        assert!(l.signed_in());
        assert!(!l.needs_refresh(10_000 - 301));
        assert!(l.needs_refresh(10_000 - 300));
        assert!(!GrokLogin::default().signed_in());
    }

    #[test]
    fn refresh_replaces_the_tokens_and_keeps_the_refresh_token_when_none_comes() {
        let (auth, got) = serve_with(1, |_| (200, r#"{"access_token":"a2","expires_in":100}"#.to_owned()));
        let mut l = GrokLogin { email: "e".into(), access_token: "a1".into(), refresh_token: "r1".into(), expires_at: 0 };
        refresh(&super::super::login::agent(), &auth, &mut l, 50).unwrap();
        assert_eq!((l.access_token.as_str(), l.refresh_token.as_str(), l.expires_at), ("a2", "r1", 150));
        let raw = &got.join().unwrap()[0];
        assert_eq!(field(raw, "grant_type").as_deref(), Some("refresh_token"));
        assert_eq!(field(raw, "refresh_token").as_deref(), Some("r1"));
        assert_eq!(field(raw, "client_id").as_deref(), Some(CLIENT_ID));
    }

    #[test]
    fn a_refused_refresh_signs_out_but_the_plan_gate_does_not() {
        let answers = [(400, r#"{"error":"invalid_grant","error_description":"Invalid or unknown refresh token"}"#), (403, r#"{"error":"forbidden"}"#)];
        let mut it = answers.into_iter();
        let (auth, got) = serve_with(2, move |_| {
            let (s, b) = it.next().unwrap();
            (s, b.to_owned())
        });
        let agent = super::super::login::agent();
        let signed_in = GrokLogin { email: "e".into(), access_token: "a".into(), refresh_token: "r".into(), expires_at: 0 };
        let mut l = signed_in.clone();
        assert_eq!(refresh(&agent, &auth, &mut l, 0).unwrap_err(), AiError::SignInAgain);
        assert!(!l.signed_in());
        let mut l = signed_in.clone();
        assert_eq!(refresh(&agent, &auth, &mut l, 0).unwrap_err(), AiError::GrokPlan);
        assert_eq!(l, signed_in);
        got.join().unwrap();
    }

    #[test]
    fn sign_out_revokes_the_refresh_token() {
        let (auth, got) = serve_with(1, |_| (200, String::new()));
        let l = GrokLogin { refresh_token: "r1".into(), ..GrokLogin::default() };
        revoke(&super::super::login::agent(), &auth, &l);
        let raw = &got.join().unwrap()[0];
        assert!(raw.starts_with("POST /oauth2/revoke "), "{raw}");
        assert_eq!(field(raw, "token").as_deref(), Some("r1"));
        assert_eq!(field(raw, "token_type_hint").as_deref(), Some("refresh_token"));
        assert_eq!(field(raw, "client_id").as_deref(), Some(CLIENT_ID));
    }
}
