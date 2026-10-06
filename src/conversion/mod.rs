//! Source composition and numeric conversion, independent of destination protocols.
pub mod config;
pub mod extraction;
mod http;

use crate::dest::{PaymentInfo, Result};
use extraction::{Extractor, Scalar};
use http::HttpGet;
use std::sync::Arc;
use std::time::Duration;

pub fn positive(value: f64) -> Result<f64> {
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        Err("value must be finite and greater than zero".into())
    }
}

pub fn numeric(value: Scalar) -> Result<f64> {
    positive(match value {
        Scalar::Number(n) => n,
        Scalar::Text(s) => s.trim().parse().map_err(|_| "invalid numeric string")?,
        _ => return Err("price must be a number or numeric string".into()),
    })
}

pub trait NumericSource: Send + Sync {
    fn value(&self) -> Result<f64>;
    fn description(&self) -> Option<SourceDescription> {
        None
    }
    fn initial_value(&self) -> Option<f64> {
        None
    }
}

struct FixedSource(f64);
impl NumericSource for FixedSource {
    fn description(&self) -> Option<SourceDescription> {
        Some(SourceDescription {
            name: "Fixed value".into(),
            fields: vec![("Value".into(), self.0.to_string())],
        })
    }
    fn value(&self) -> Result<f64> {
        Ok(self.0)
    }
    fn initial_value(&self) -> Option<f64> {
        Some(self.0)
    }
}

struct RetrievedSource {
    retrieval: HttpGet,
    extractor: Box<dyn Extractor>,
    description: SourceDescription,
}
impl NumericSource for RetrievedSource {
    fn description(&self) -> Option<SourceDescription> {
        Some(self.description.clone())
    }
    fn value(&self) -> Result<f64> {
        numeric(self.extractor.extract(&self.retrieval.get()?)?)
    }
}

/// Source-owned metadata; consumers do not need to understand source formats.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceDescription {
    pub name: String,
    pub fields: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConversionSettings {
    pub currency: String,
    pub multiplier: f64,
    pub source: Option<SourceDescription>,
}

pub trait PaymentConversion: Send + Sync {
    fn payment_info(&self) -> Result<PaymentInfo>;
    fn initial_payment(&self) -> Result<Option<PaymentInfo>>;
    fn cache_duration(&self) -> Duration;
    fn settings(&self) -> Option<ConversionSettings> {
        None
    }
}

pub struct Conversion {
    currency: String,
    multiplier: f64,
    source: Arc<dyn NumericSource>,
    cache: Duration,
}

impl Conversion {
    pub fn new(
        currency: String,
        multiplier: f64,
        source: Arc<dyn NumericSource>,
        cache: Duration,
    ) -> Result<Self> {
        if currency.trim().is_empty() {
            return Err("currency must not be empty".into());
        }
        positive(multiplier)?;
        let conversion = Self {
            currency,
            multiplier,
            source,
            cache,
        };
        conversion.initial_payment()?;
        Ok(conversion)
    }

    fn convert(&self, value: f64) -> Result<PaymentInfo> {
        let exchange_rate = positive(positive(value)? * self.multiplier)?;
        let info = PaymentInfo {
            payment_currency: self.currency.clone(),
            exchange_rate,
        };
        info.validate()?;
        Ok(info)
    }
}

impl PaymentConversion for Conversion {
    fn settings(&self) -> Option<ConversionSettings> {
        Some(ConversionSettings {
            currency: self.currency.clone(),
            multiplier: self.multiplier,
            source: self.source.description(),
        })
    }
    fn payment_info(&self) -> Result<PaymentInfo> {
        self.convert(self.source.value()?)
    }
    fn initial_payment(&self) -> Result<Option<PaymentInfo>> {
        self.source
            .initial_value()
            .map(|v| self.convert(v))
            .transpose()
    }
    fn cache_duration(&self) -> Duration {
        self.cache
    }
}
