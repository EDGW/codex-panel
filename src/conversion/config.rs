//! Conversion configuration parsing; tagged-source boundaries stay here.
use super::extraction::{
    Condition, JsonExtractor, Scalar, XmlExtractor, validate_pointer, validate_xpath,
};
use super::http::HttpGet;
use super::{
    Conversion, FixedSource, PaymentConversion, RetrievedSource, SourceDescription, positive,
};
use crate::dest::Result;
use crate::url::http_url;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialConversion {
    enabled: Option<bool>,
    currency: Option<String>,
    multiplier: Option<f64>,
    source: Option<toml::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialSource {
    #[serde(rename = "type")]
    kind: Option<String>,
    value: Option<f64>,
    url: Option<String>,
    pointer: Option<String>,
    xpath: Option<String>,
    namespaces: Option<BTreeMap<String, String>>,
    cache_seconds: Option<u64>,
    timeout_seconds: Option<u64>,
    expect: Option<toml::Value>,
}

/// Complete source configs are tagged and cannot mix fields across formats.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
enum SourceConfig {
    Value {
        value: f64,
    },
    Json {
        url: String,
        pointer: String,
        cache_seconds: Option<u64>,
        timeout_seconds: Option<u64>,
        expect: Option<toml::Value>,
    },
    Xml {
        url: String,
        xpath: String,
        namespaces: Option<BTreeMap<String, String>>,
        cache_seconds: Option<u64>,
        timeout_seconds: Option<u64>,
        expect: Option<toml::Value>,
    },
}

impl SourceConfig {
    fn description(&self) -> SourceDescription {
        let (name, url, expression, cache, timeout, expect, namespaces) = match self {
            Self::Value { value } => {
                return SourceDescription {
                    name: "Fixed value".into(),
                    fields: vec![("Value".into(), value.to_string())],
                };
            }
            Self::Json {
                url,
                pointer,
                cache_seconds,
                timeout_seconds,
                expect,
            } => (
                "JSON",
                url,
                ("JSON Pointer", pointer),
                cache_seconds,
                timeout_seconds,
                expect,
                None,
            ),
            Self::Xml {
                url,
                xpath,
                cache_seconds,
                timeout_seconds,
                expect,
                namespaces,
            } => (
                "XML",
                url,
                ("XPath", xpath),
                cache_seconds,
                timeout_seconds,
                expect,
                namespaces.as_ref(),
            ),
        };
        let mut fields = vec![
            ("URL".into(), url.clone()),
            (expression.0.into(), expression.1.clone()),
            (
                "Cache / timeout".into(),
                format!("{}s / {}s", cache.unwrap_or(300), timeout.unwrap_or(10)),
            ),
        ];
        if let Some(namespaces) = namespaces {
            fields.extend(
                namespaces
                    .iter()
                    .map(|(prefix, uri)| (format!("Namespace {prefix}"), uri.clone())),
            );
        }
        if let Some(expect) = expect
            && let Some(table) = expect.as_table()
        {
            let expression = table
                .get("pointer")
                .or_else(|| table.get("xpath"))
                .and_then(toml::Value::as_str)
                .expect("validated condition expression");
            fields.push((
                "Expect".into(),
                format!("{expression} = {}", table["equals"]),
            ));
        }
        SourceDescription {
            name: name.into(),
            fields,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialCondition {
    pointer: Option<String>,
    xpath: Option<String>,
    equals: Option<toml::Value>,
}

fn condition(
    value: Option<&toml::Value>,
    kind: Option<&str>,
    complete: bool,
) -> Result<Option<Condition>> {
    let Some(value) = value else { return Ok(None) };
    if value.as_bool() == Some(false) {
        return Ok(None);
    }
    let parsed: PartialCondition = value
        .clone()
        .try_into()
        .map_err(|e| format!("expect: {e}"))?;
    if let Some(pointer) = &parsed.pointer {
        validate_pointer(pointer).map_err(|e| format!("expect.pointer: {e}"))?;
    }
    if let Some(xpath) = &parsed.xpath {
        validate_xpath(xpath).map_err(|e| format!("expect.xpath: {e}"))?;
    }
    let expression = match kind {
        Some("json") => {
            if parsed.xpath.is_some() {
                return Err("expect.xpath: unknown field for JSON source".into());
            }
            parsed.pointer
        }
        Some("xml") => {
            if parsed.pointer.is_some() {
                return Err("expect.pointer: unknown field for XML source".into());
            }
            parsed.xpath
        }
        _ => parsed.pointer.or(parsed.xpath),
    };
    let equals = parsed
        .equals
        .map(|v| -> Result<Scalar> {
            let json = serde_json::to_value(v).map_err(|e| format!("expect.equals: {e}"))?;
            let scalar = Scalar::from_json(&json).map_err(|e| format!("expect.equals: {e}"))?;
            if matches!(scalar, Scalar::Number(n) if !n.is_finite()) || scalar == Scalar::Null {
                return Err("expect.equals: must be a finite scalar".into());
            }
            Ok(scalar)
        })
        .transpose()?;
    match (expression, equals) {
        (Some(expression), Some(equals)) => Ok(Some(Condition { expression, equals })),
        _ if complete => Err("expect: requires an expression and equals".into()),
        _ => Ok(None),
    }
}

fn parse_source(value: &toml::Value, complete: bool) -> Result<PartialSource> {
    let source: PartialSource = value.clone().try_into().map_err(|e| e.to_string())?;
    let kind = source.kind.as_deref();
    if let Some(kind) = kind {
        let allowed: &[&str] = match kind {
            "value" => &["type", "value"],
            "json" => &[
                "type",
                "url",
                "pointer",
                "cache_seconds",
                "timeout_seconds",
                "expect",
            ],
            "xml" => &[
                "type",
                "url",
                "xpath",
                "namespaces",
                "cache_seconds",
                "timeout_seconds",
                "expect",
            ],
            _ => return Err(format!("type: unknown conversion source type '{kind}'")),
        };
        for key in value.as_table().ok_or("must be a table")?.keys() {
            if !allowed.contains(&key.as_str()) {
                return Err(format!("{key}: unknown field for {kind} source"));
            }
        }
    } else if complete {
        return Err("type: missing required field".into());
    }
    if let Some(value) = source.value {
        positive(value).map_err(|e| format!("value: {e}"))?;
    }
    if let Some(url) = &source.url {
        http_url(url, true).map_err(|e| format!("url: {e}"))?;
    }
    if let Some(pointer) = &source.pointer {
        validate_pointer(pointer).map_err(|e| format!("pointer: {e}"))?;
    }
    if let Some(xpath) = &source.xpath {
        validate_xpath(xpath).map_err(|e| format!("xpath: {e}"))?;
    }
    if let Some(namespaces) = &source.namespaces {
        for (prefix, uri) in namespaces {
            let mut chars = prefix.chars();
            if !chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
                || !chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
                || uri.trim().is_empty()
            {
                return Err(format!(
                    "namespaces.{prefix}: invalid namespace prefix or URI"
                ));
            }
        }
    }
    if source.timeout_seconds == Some(0) {
        return Err("timeout_seconds: must be greater than zero".into());
    }
    for (field, seconds) in [
        ("cache_seconds", source.cache_seconds),
        ("timeout_seconds", source.timeout_seconds),
    ] {
        if seconds.is_some_and(|s| Instant::now().checked_add(Duration::from_secs(s)).is_none()) {
            return Err(format!("{field}: duration is out of range"));
        }
    }
    condition(source.expect.as_ref(), kind, complete)?;
    if complete {
        match kind {
            Some("value") if source.value.is_none() => {
                return Err("value: missing required field".into());
            }
            Some("json" | "xml") => {
                if source.url.is_none() {
                    return Err("url: missing required field".into());
                }
                if kind == Some("json") && source.pointer.is_none() {
                    return Err("pointer: missing required field".into());
                }
                if kind == Some("xml") && source.xpath.is_none() {
                    return Err("xpath: missing required field".into());
                }
            }
            _ => {}
        }
    }
    Ok(source)
}

/// Validate supplied fields even when disabled. Only enabled, complete blocks create capabilities.
pub fn parse(value: &toml::Value, complete: bool) -> Result<Option<Arc<dyn PaymentConversion>>> {
    let config: PartialConversion = value
        .clone()
        .try_into()
        .map_err(|e| format!("conversion: {e}"))?;
    if config
        .currency
        .as_ref()
        .is_some_and(|v| v.trim().is_empty())
    {
        return Err("conversion.currency: must not be empty".into());
    }
    let multiplier = config.multiplier.unwrap_or(1.0);
    positive(multiplier).map_err(|e| format!("conversion.multiplier: {e}"))?;
    let enabled = config.enabled.unwrap_or(true);
    let source = config
        .source
        .as_ref()
        .map(|s| parse_source(s, complete && enabled).map_err(|e| format!("conversion.source.{e}")))
        .transpose()?;
    if let Some(source) = &source
        && let Some(value) = source.value
    {
        positive(value * multiplier).map_err(|e| format!("conversion.multiplier: {e}"))?;
    }
    if !complete || !enabled {
        return Ok(None);
    }
    let currency = config
        .currency
        .ok_or("conversion.currency: missing required field")?;
    let typed: SourceConfig = config
        .source
        .ok_or("conversion.source: missing required field")?
        .try_into()
        .map_err(|e| format!("conversion.source: {e}"))?;
    let description = typed.description();
    let (source, cache): (Arc<dyn super::NumericSource>, Duration) = match typed {
        SourceConfig::Value { value } => (Arc::new(FixedSource(value)), Duration::ZERO),
        SourceConfig::Json {
            url,
            pointer,
            cache_seconds,
            timeout_seconds,
            expect,
        } => {
            let expect = condition(expect.as_ref(), Some("json"), true)
                .map_err(|e| format!("conversion.source.{e}"))?;
            retrieved(
                url,
                timeout_seconds,
                cache_seconds,
                Box::new(JsonExtractor { pointer, expect }),
                description,
            )?
        }
        SourceConfig::Xml {
            url,
            xpath,
            namespaces,
            cache_seconds,
            timeout_seconds,
            expect,
        } => {
            let expect = condition(expect.as_ref(), Some("xml"), true)
                .map_err(|e| format!("conversion.source.{e}"))?;
            retrieved(
                url,
                timeout_seconds,
                cache_seconds,
                Box::new(XmlExtractor {
                    xpath,
                    namespaces: namespaces.unwrap_or_default(),
                    expect,
                }),
                description,
            )?
        }
    };
    Ok(Some(Arc::new(
        Conversion::new(currency, multiplier, source, cache)
            .map_err(|e| format!("conversion: {e}"))?,
    )))
}

fn retrieved(
    url: String,
    timeout: Option<u64>,
    cache: Option<u64>,
    extractor: Box<dyn super::extraction::Extractor>,
    description: SourceDescription,
) -> Result<(Arc<dyn super::NumericSource>, Duration)> {
    let source = RetrievedSource {
        retrieval: HttpGet::new(
            http_url(&url, true)?,
            Duration::from_secs(timeout.unwrap_or(10)),
        )?,
        extractor,
        description,
    };
    Ok((Arc::new(source), Duration::from_secs(cache.unwrap_or(300))))
}
