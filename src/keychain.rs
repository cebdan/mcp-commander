use anyhow::{Context, Result};
use security_framework::passwords::{
    PasswordOptions, delete_generic_password_options, generic_password,
    set_generic_password_options,
};
use security_framework_sys::base::errSecItemNotFound;

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
