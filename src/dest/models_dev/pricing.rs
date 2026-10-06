//! models.dev provider/model catalog and USD token-price decoding.
use crate::dest::{Result, TokenPrices, TokenQuote};
use crate::source::{extraction::json, numeric::nonnegative};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
struct Provider {
    models: BTreeMap<String, Model>,
}

#[derive(Deserialize)]
struct Model {
    name: String,
    cost: Option<Cost>,
}

#[derive(Deserialize)]
struct Cost {
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
    reasoning: Option<f64>,
}

pub(super) struct Catalog {
    models: BTreeMap<String, Model>,
}

pub(super) struct Selection {
    pub quote: TokenQuote,
    pub model: String,
}

impl Catalog {
    pub fn decode(body: &str, provider: &str) -> Result<Self> {
        let dataset: BTreeMap<String, serde_json::Value> = json(body)?;
        let value = dataset
            .get(provider)
            .ok_or_else(|| format!("provider '{provider}' is not in the price source"))?;
        let provider: Provider = serde_json::from_value(value.clone())
            .map_err(|error| format!("provider '{provider}': {error}"))?;
        if provider.models.is_empty() {
            return Err("provider has no models in the price source".into());
        }
        Ok(Self {
            models: provider.models,
        })
    }

    pub fn select(&self, model: &str) -> Result<Selection> {
        // Only API identifiers are keys. Display names are not unique identifiers.
        let entry = self.models.get(model).ok_or_else(|| format!(
            "model '{model}' is not in this provider's dataset; configure model_aliases for an explicit API identifier"
        ))?;
        let cost = entry
            .cost
            .as_ref()
            .ok_or_else(|| format!("model '{model}' has no token prices"))?;
        let input = cost
            .input
            .ok_or_else(|| format!("model '{model}' cost.input: missing token price"))?;
        let output = cost
            .output
            .ok_or_else(|| format!("model '{model}' cost.output: missing token price"))?;
        let rate = |value, field| {
            nonnegative(value).map_err(|error| format!("model '{model}' cost.{field}: {error}"))
        };
        Ok(Selection {
            quote: TokenQuote {
                currency: "USD".into(),
                prices: TokenPrices {
                    input: rate(input, "input")?,
                    output: rate(output, "output")?,
                    cached_input: rate(cost.cache_read.unwrap_or(input), "cache_read")?,
                    cache_write_input: rate(cost.cache_write.unwrap_or(input), "cache_write")?,
                    reasoning_output: rate(cost.reasoning.unwrap_or(output), "reasoning")?,
                },
            },
            model: entry.name.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn provider_and_exact_api_id_select_usd_prices_and_optional_rates_fall_back() {
        let body = json!({
            "example": {"models": {
                "lab/model": {"name": "Friendly name", "cost": {"input": 0.15, "output": 0.6, "cache_read": 0.003}},
                "full": {"name": "Full", "cost": {"input": 2, "output": 8, "cache_read": 0.2, "cache_write": 3, "reasoning": 10}},
                "missing": {"name": "Missing", "cost": {"input": 1}},
                "unpriced": {"name": "Unpriced"},
                "invalid": {"name": "Invalid", "cost": {"input": -1, "output": 2}}
            }},
            "other": {"models": {"lab/model": {"name": "Friendly name", "cost": {"input": 99, "output": 999}}}},
            "unrelated": {"models": "unrecognized data"}
        }).to_string();
        let catalog = Catalog::decode(&body, "example").unwrap();
        let selected = catalog.select("lab/model").unwrap();
        assert_eq!(selected.quote.currency, "USD");
        assert_eq!(
            selected.quote.prices,
            TokenPrices {
                input: 0.15,
                output: 0.6,
                cached_input: 0.003,
                cache_write_input: 0.15,
                reasoning_output: 0.6
            }
        );
        let full = catalog.select("full").unwrap().quote.prices;
        assert_eq!((full.cache_write_input, full.reasoning_output), (3.0, 10.0));
        for model in ["Friendly name", "unknown", "missing", "unpriced", "invalid"] {
            assert!(catalog.select(model).is_err(), "{model}");
        }
        assert!(Catalog::decode(&body, "absent").is_err());
        assert!(Catalog::decode("not json", "example").is_err());
    }
}
