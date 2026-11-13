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
    let v: Value = serde_json::from_str(body).map_err(|e| AiError::BadReply(format!("tokens: {e}")))?;
    let s = |k: &str| v[k].as_str().unwrap_or("").to_owned();
    let access_token = s("access_token");
    if access_token.is_empty() {
        return Err(AiError::BadReply("no access token".into()));
    }
    let scopes: Vec<String> = v["scope"].as_str().unwrap_or("").split_whitespace().map(str::to_owned).collect();
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

#[cfg(test)]
mod tests {
    use base64::Engine;

    use super::*;

    fn pending() -> Pending {
        Pending { state: "st".into(), nonce: "no".into(), verifier: "v".repeat(43), redirect: "http://127.0.0.1:1455/auth/callback".into() }
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
