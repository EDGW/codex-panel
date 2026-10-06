use crate::dest::{Credentials, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub struct CodexCredentials {
    home: PathBuf,
}

impl CodexCredentials {
    pub(crate) fn at(home: PathBuf) -> Self {
        Self { home }
    }

    pub fn discover() -> Result<Self> {
        let home = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
            .ok_or("cannot locate Codex home")?;
        Ok(Self::at(home))
    }

    pub fn profile(&self, profile: Option<&str>) -> Result<String> {
        let config = load_config(&self.home)?;
        Ok(profile
            .or_else(|| config.get("model_provider").and_then(toml::Value::as_str))
            .unwrap_or("openai")
            .to_owned())
    }

    pub fn api_url(&self, profile: Option<&str>) -> Result<String> {
        api_url_from_config(&load_config(&self.home)?, profile, |name| {
            std::env::var(name).ok()
        })
    }
}

impl Credentials for CodexCredentials {
    fn api_key(&self, profile: Option<&str>) -> Result<String> {
        load_key(&self.home, profile)
    }
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn load_key(home: &Path, provider: Option<&str>) -> Result<String> {
    if let Some(key) = nonempty(std::env::var("PREVX_API_KEY").ok()) {
        return Ok(key);
    }
    key_from_config(home, &load_config(home)?, provider, |name| {
        std::env::var(name).ok()
    })
}

fn load_config(home: &Path) -> Result<toml::Value> {
    let config_path = home.join("config.toml");
    if config_path.exists() {
        std::fs::read_to_string(config_path)
            .map_err(|_| "cannot read Codex config.toml")?
            .parse()
            .map_err(|_| "cannot parse Codex config.toml".into())
    } else {
        Ok(toml::Value::Table(Default::default()))
    }
}

fn api_url_from_config(
    config: &toml::Value,
    profile: Option<&str>,
    env: impl Fn(&str) -> Option<String>,
) -> Result<String> {
    let profile = profile
        .or_else(|| config.get("model_provider").and_then(toml::Value::as_str))
        .unwrap_or("openai");
    if let Some(url) = config
        .get("model_providers")
        .and_then(|providers| providers.get(profile))
        .and_then(|info| info.get("base_url"))
        .and_then(toml::Value::as_str)
    {
        return nonempty(Some(url.into())).ok_or_else(|| "provider base_url is empty".into());
    }
    if profile == "openai" {
        return Ok(
            nonempty(env("OPENAI_BASE_URL")).unwrap_or_else(|| "https://api.openai.com/v1".into())
        );
    }
    Err(format!(
        "provider {profile} has no base_url in Codex config"
    ))
}

fn key_from_config(
    home: &Path,
    config: &toml::Value,
    provider: Option<&str>,
    env: impl Fn(&str) -> Option<String>,
) -> Result<String> {
    let provider = provider.or_else(|| config.get("model_provider").and_then(toml::Value::as_str));
    let info = provider.and_then(|name| config.get("model_providers")?.get(name));
    if let Some(name) = info
        .and_then(|info| info.get("env_key"))
        .and_then(toml::Value::as_str)
    {
        return nonempty(env(name)).ok_or_else(|| format!("provider env_key {name} is not set"));
    }
    if let Some(key) = info
        .and_then(|info| info.get("experimental_bearer_token"))
        .and_then(toml::Value::as_str)
        .filter(|key| !key.trim().is_empty())
    {
        return Ok(key.to_owned());
    }
    let data = std::fs::read(home.join("auth.json"))
        .map_err(|_| "no API key: check Codex auth.json or PREVX_API_KEY")?;
    let auth: Value = serde_json::from_slice(&data).map_err(|_| "cannot parse Codex auth.json")?;
    nonempty(
        auth.get("OPENAI_API_KEY")
            .and_then(Value::as_str)
            .map(str::to_owned),
    )
    .ok_or_else(|| "Codex auth has no API key; set PREVX_API_KEY for relay login".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn api_url_uses_the_current_session_provider_before_the_config_default() {
        let config: toml::Value = "model_provider='a'\n[model_providers.a]\nbase_url='https://a.example/v1'\n[model_providers.b]\nbase_url='https://b.example/v1'".parse().unwrap();
        assert_eq!(
            api_url_from_config(&config, None, |_| None).unwrap(),
            "https://a.example/v1"
        );
        assert_eq!(
            api_url_from_config(&config, Some("b"), |_| None).unwrap(),
            "https://b.example/v1"
        );
        assert!(api_url_from_config(&config, Some("unknown"), |_| None).is_err());
        let empty = toml::Value::Table(Default::default());
        assert_eq!(
            api_url_from_config(&empty, None, |_| Some("https://openai.example/v1".into()))
                .unwrap(),
            "https://openai.example/v1"
        );
    }
    #[test]
    fn provider_specific_env_key_precedes_auth_file_and_missing_env_is_explicit() {
        let config: toml::Value =
            "model_provider='relay'\n[model_providers.relay]\nenv_key='RELAY_KEY'"
                .parse()
                .unwrap();
        assert_eq!(
            key_from_config(Path::new("/nonexistent"), &config, None, |name| (name
                == "RELAY_KEY")
                .then(|| "test-key".into()))
            .unwrap(),
            "test-key"
        );
        assert!(
            key_from_config(Path::new("/nonexistent"), &config, None, |_| None)
                .unwrap_err()
                .contains("env_key RELAY_KEY")
        );
    }
}
