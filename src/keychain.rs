use anyhow::Result;

use crate::provider::Provider;

const SERVICE: &str = "local.ollama-mcp-agent.api-key";

fn account(provider: Provider) -> Option<&'static str> {
    match provider {
        Provider::Ollama => None,
        Provider::OpenAI => Some("openai"),
        Provider::Anthropic => Some("anthropic"),
        Provider::Gemini => Some("gemini"),
        Provider::DeepSeek => Some("deepseek"),
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use anyhow::Context;
    use security_framework::passwords::{
        PasswordOptions, delete_generic_password_options, generic_password,
        set_generic_password_options,
    };
    use security_framework_sys::base::errSecItemNotFound;

    fn options(account: &str) -> PasswordOptions {
        let mut options = PasswordOptions::new_generic_password(SERVICE, account);
        options.set_access_synchronized(Some(false));
        options
    }

    pub fn load(provider: Provider) -> Result<Option<String>> {
        let Some(account) = account(provider) else { return Ok(None); };
        match generic_password(options(account)) {
            Ok(bytes) => Ok(Some(String::from_utf8(bytes).context("saved API key is not UTF-8")?)),
            Err(error) if error.code() == errSecItemNotFound => Ok(None),
            Err(error) => Err(error).context("macOS Keychain lookup failed"),
        }
    }

    pub fn save(provider: Provider, key: &str) -> Result<()> {
        let Some(account) = account(provider) else { return Ok(()); };
        set_generic_password_options(key.as_bytes(), options(account))
            .context("macOS Keychain write failed")
    }

    pub fn delete(provider: Provider) -> Result<()> {
        let Some(account) = account(provider) else { return Ok(()); };
        match delete_generic_password_options(options(account)) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == errSecItemNotFound => Ok(()),
            Err(error) => Err(error).context("macOS Keychain deletion failed"),
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use anyhow::Context;
    use keyring::{Entry, Error};

    fn entry(account: &str) -> Result<Entry> {
        Entry::new(SERVICE, account).context("Windows Credential Manager is unavailable")
    }

    pub fn load(provider: Provider) -> Result<Option<String>> {
        let Some(account) = account(provider) else { return Ok(None); };
        match entry(account)?.get_password() {
            Ok(key) => Ok(Some(key)),
            Err(Error::NoEntry) => Ok(None),
            Err(error) => Err(error).context("Windows Credential Manager lookup failed"),
        }
    }

    pub fn save(provider: Provider, key: &str) -> Result<()> {
        let Some(account) = account(provider) else { return Ok(()); };
        entry(account)?.set_password(key).context("Windows Credential Manager write failed")
    }

    pub fn delete(provider: Provider) -> Result<()> {
        let Some(account) = account(provider) else { return Ok(()); };
        match entry(account)?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(error) => Err(error).context("Windows Credential Manager deletion failed"),
        }
    }
}

pub use platform::{delete, load, save};
