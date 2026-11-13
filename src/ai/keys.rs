//! API keys, one per provider, in `ai-keys.toml` beside `config.toml`.
//!
//! They are kept out of `config.toml` because people share that file. The
//! file is readable by the user only (on Unix; on Windows it sits in the
//! user's own profile). A provider's environment variable is used when the
//! file has no key for it.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use super::Provider;
use crate::config::Config;

/// The keys from `ai-keys.toml`, by provider name.
#[derive(Debug, Clone, Default)]
pub struct Keys {
    map: BTreeMap<String, String>,
}

impl Keys {
    /// Where the keys are kept: beside the settings file, so the portable
    /// build keeps them in its own folder.
    pub fn path() -> Option<PathBuf> {
        Some(Config::path()?.with_file_name("ai-keys.toml"))
    }

    /// Reads `path`. A missing or broken file has no keys; a broken one is
    /// logged (without its content: it holds secrets).
    pub fn load(path: &Path) -> Keys {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Keys::default(),
            Err(e) => {
                log::warn!("reading {}: {e}", path.display());
                return Keys::default();
            }
        };
        match toml::from_str::<BTreeMap<String, String>>(&text) {
            Ok(map) => Keys { map: map.into_iter().filter(|(_, v)| !v.trim().is_empty()).collect() },
            Err(_) => {
                log::warn!("{} is not valid: its keys are ignored", path.display());
                Keys::default()
            }
        }
    }

    /// The key for `p`: from the file, else from its environment variable.
    pub fn get(&self, p: Provider) -> Option<String> {
        self.in_file(p).map(str::to_owned).or_else(|| self.in_env(p))
    }

    /// The key the file has for `p`.
    pub fn in_file(&self, p: Provider) -> Option<&str> {
        self.map.get(p.name()).map(String::as_str)
    }

    /// The key `p`'s environment variable has.
    pub fn in_env(&self, p: Provider) -> Option<String> {
        p.env_var().and_then(|v| std::env::var(v).ok()).filter(|k| !k.trim().is_empty())
    }

    /// Sets or removes (None) the key for `p`.
    pub fn set(&mut self, p: Provider, key: Option<String>) {
        match key.filter(|k| !k.trim().is_empty()) {
            Some(k) => {
                self.map.insert(p.name().to_owned(), k.trim().to_owned());
            }
            None => {
                self.map.remove(p.name());
            }
        }
    }

    /// Writes the whole file, readable by the user only.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let body = toml::to_string(&self.map).map_err(io::Error::other)?;
        let text = format!("# fastcnsl API keys (Options → AI). Keep this file private.\n{body}");
        crate::config::write_private(path, &text)
    }
}

/// A key as Options shows it: its first 3 and last 4 characters.
pub fn mask(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() < 10 {
        let tail: String = chars[chars.len().saturating_sub(2)..].iter().collect();
        return format!("…{tail}");
    }
    let head: String = chars[..3].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::Provider;
    use crate::testterm::temp_dir;

    #[test]
    fn keys_saved_privately_and_read_back() {
        let dir = temp_dir("keys");
        let path = dir.join("ai-keys.toml");
        let mut k = Keys::default();
        k.set(Provider::Xai, Some("xai-123456789".into()));
        k.save(&path).unwrap();
        assert_eq!(Keys::load(&path).get(Provider::Xai).as_deref(), Some("xai-123456789"));
        #[cfg(unix)]
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&path).unwrap().permissions()) & 0o777, 0o600);
        k.set(Provider::Xai, None);
        k.save(&path).unwrap();
        assert_eq!(Keys::load(&path).map.get("xai"), None);
    }

    #[test]
    fn broken_key_file_means_no_keys() {
        let dir = temp_dir("keys-broken");
        let path = dir.join("ai-keys.toml");
        for text in ["", "not = [toml", "openai = 5"] {
            std::fs::write(&path, text).unwrap();
            assert!(Keys::load(&path).map.is_empty(), "{text}");
        }
        assert!(Keys::load(&dir.join("missing.toml")).map.is_empty());
    }

    #[test]
    fn masked_keys_show_only_the_ends() {
        assert_eq!(mask("sk-ant-api03-abcdefwxyz"), "sk-…wxyz");
        assert_eq!(mask("short"), "…rt");
    }
}
