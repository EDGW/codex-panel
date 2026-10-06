//! Type registration, validated factories and exact API routing; no protocol knowledge.
use super::{Credentials, Destination, Result};
use crate::url::http_url;
use std::collections::HashMap;
use std::sync::Arc;

pub struct CreateContext {
    pub credentials: Arc<dyn Credentials>,
    pub name: String,
}

pub trait DestinationFactory: Send + Sync {
    fn create(&self, context: CreateContext) -> Result<Arc<dyn Destination>>;

    /// Nonfatal limitations to report at startup and for the selected instance.
    fn warnings(&self) -> Vec<&str> {
        Vec::new()
    }
}

/// Partial validation accepts missing fields, but rejects supplied invalid fields.
pub type ConfigParser =
    dyn Fn(&toml::Value, bool) -> Result<Option<Arc<dyn DestinationFactory>>> + Send + Sync;

#[derive(Default)]
pub struct Registry {
    parsers: HashMap<String, Arc<ConfigParser>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        kind: &str,
        parser: impl Fn(&toml::Value, bool) -> Result<Option<Arc<dyn DestinationFactory>>>
        + Send
        + Sync
        + 'static,
    ) -> Result<()> {
        validate_id(kind)?;
        if self.parsers.contains_key(kind) {
            return Err(format!("destination type already registered: {kind}"));
        }
        self.parsers.insert(kind.into(), Arc::new(parser));
        Ok(())
    }

    pub fn parse(
        &self,
        kind: &str,
        value: &toml::Value,
        complete: bool,
    ) -> Result<Option<Arc<dyn DestinationFactory>>> {
        self.parsers
            .get(kind)
            .ok_or_else(|| format!("type: unregistered destination type: {kind}"))?(
            value, complete
        )
    }
}

pub fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-'))
    {
        return Err(
            "id/type must contain lowercase letters, digits, underscores or hyphens".into(),
        );
    }
    Ok(())
}

#[derive(Default)]
pub struct DestinationMappings {
    routes: HashMap<String, String>,
}

impl DestinationMappings {
    pub fn insert(&mut self, api_url: &str, id: &str) -> Result<()> {
        let key = normalize_url(api_url)?;
        if let Some(previous) = self.routes.get(&key) {
            return Err(format!(
                "api_urls: duplicate API URL (also belongs to instance '{previous}')"
            ));
        }
        self.routes.insert(key, id.into());
        Ok(())
    }

    pub fn destination_id(&self, api_url: &str) -> Result<&str> {
        self.routes
            .get(&normalize_url(api_url)?)
            .map(String::as_str)
            .ok_or_else(|| format!("Unrecognized Destination: {api_url}"))
    }
}

pub fn normalize_url(value: &str) -> Result<String> {
    let url = http_url(value, false)?;
    Ok(url.as_str().trim_end_matches('/').to_owned())
}
