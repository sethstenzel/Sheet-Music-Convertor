//! Credentials (Claude OAuth token, Anthropic API key) stored in the OS
//! credential manager (Windows Credential Manager, macOS Keychain, Secret
//! Service). Falls back to a file in the config directory only when no
//! credential store is available.

use anyhow::{anyhow, Result};

const SERVICE: &str = "sheet-music-convertor";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Secret {
    /// Long-lived token from `claude setup-token` (Claude subscription).
    ClaudeOauthToken,
    /// Anthropic Console API key.
    AnthropicApiKey,
}

impl Secret {
    fn account(self) -> &'static str {
        match self {
            Secret::ClaudeOauthToken => "claude-oauth-token",
            Secret::AnthropicApiKey => "anthropic-api-key",
        }
    }

    fn fallback_path(self) -> std::path::PathBuf {
        crate::config::config_dir().join(format!(".{}", self.account()))
    }
}

pub fn get(secret: Secret) -> Option<String> {
    if let Ok(entry) = keyring::Entry::new(SERVICE, secret.account()) {
        if let Ok(v) = entry.get_password() {
            if !v.trim().is_empty() {
                return Some(v);
            }
        }
    }
    std::fs::read_to_string(secret.fallback_path()).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Store a secret. Returns `true` when it went to the OS credential store,
/// `false` when the file fallback was used.
pub fn set(secret: Secret, value: &str) -> Result<bool> {
    let value = value.trim();
    if value.is_empty() {
        return Err(anyhow!("empty value"));
    }
    if let Ok(entry) = keyring::Entry::new(SERVICE, secret.account()) {
        if entry.set_password(value).is_ok() && entry.get_password().ok().as_deref() == Some(value) {
            let _ = std::fs::remove_file(secret.fallback_path());
            return Ok(true);
        }
    }
    std::fs::create_dir_all(crate::config::config_dir())?;
    std::fs::write(secret.fallback_path(), value)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(secret.fallback_path(), std::fs::Permissions::from_mode(0o600));
    }
    Ok(false)
}

pub fn delete(secret: Secret) {
    if let Ok(entry) = keyring::Entry::new(SERVICE, secret.account()) {
        let _ = entry.delete_credential();
    }
    let _ = std::fs::remove_file(secret.fallback_path());
}

/// "sk-ant-api03-abcd…wxyz" style masked display.
pub fn mask(value: &str) -> String {
    let v = value.trim();
    if v.len() <= 16 {
        return "••••".into();
    }
    format!("{}…{}", &v[..12], &v[v.len() - 4..])
}
