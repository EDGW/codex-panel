//! ModelsDev configuration parsing. No generic discovery or accounting rules belong here.
use super::ModelsDev;
use crate::dest::{
    Destination, Result,
    registry::{CreateContext, DestinationFactory},
};
use crate::source::http::HttpGet;
use crate::url::http_url;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialConfig {
    provider_id: Option<String>,
    note: Option<String>,
    source_url: Option<String>,
    cache_seconds: Option<u64>,
    timeout_seconds: Option<u64>,
    model_aliases: Option<BTreeMap<String, String>>,
}

#[derive(Clone)]
pub(super) struct Config {
    pub provider_id: String,
    pub note: Option<String>,
    pub source_url: String,
    pub cache: Duration,
    pub timeout: Duration,
    pub aliases: BTreeMap<String, String>,
}

impl DestinationFactory for Config {
    fn create(&self, context: CreateContext) -> Result<Arc<dyn Destination>> {
        let retrieval = HttpGet::new(http_url(&self.source_url, true)?, self.timeout)?;
        Ok(Arc::new(ModelsDev::new(
            context.name,
            self.clone(),
            Arc::new(retrieval),
        )))
    }

    fn warnings(&self) -> Vec<&str> {
        self.note.as_deref().into_iter().collect()
    }
}

pub(super) fn parse(
    value: &toml::Value,
    complete: bool,
) -> Result<Option<Arc<dyn DestinationFactory>>> {
    let partial: PartialConfig = value
        .clone()
        .try_into()
        .map_err(|error| format!("config: {error}"))?;
    if let Some(id) = &partial.provider_id
        && (id.is_empty() || id.chars().any(|c| c.is_whitespace() || c.is_control()))
    {
        return Err(
            "config.provider_id: expected a nonempty provider identifier without whitespace".into(),
        );
    }
    // Protocol-defined export endpoint; custom deployments and mirrors can override it.
    let source_url = partial
        .source_url
        .unwrap_or_else(|| "https://models.dev/api.json".into());
    http_url(&source_url, true).map_err(|error| format!("config.source_url: {error}"))?;
    let cache = partial.cache_seconds.unwrap_or(300);
    let timeout = partial.timeout_seconds.unwrap_or(10);
    if timeout == 0 {
        return Err("config.timeout_seconds: must be greater than zero".into());
    }
    for (field, seconds) in [("cache_seconds", cache), ("timeout_seconds", timeout)] {
        if Instant::now()
            .checked_add(Duration::from_secs(seconds))
            .is_none()
        {
            return Err(format!("config.{field}: duration is out of range"));
        }
    }
    let aliases = partial.model_aliases.unwrap_or_default();
    for (alias, target) in &aliases {
        if alias.trim().is_empty() || target.trim().is_empty() {
            return Err(format!(
                "config.model_aliases.{alias}: alias and model identifier must not be empty"
            ));
        }
    }
    if !complete {
        return Ok(None);
    }
    Ok(Some(Arc::new(Config {
        provider_id: partial
            .provider_id
            .ok_or("config.provider_id: missing required field")?,
        note: partial.note.filter(|note| !note.trim().is_empty()),
        source_url,
        cache: Duration::from_secs(cache),
        timeout: Duration::from_secs(timeout),
        aliases,
    })))
}
