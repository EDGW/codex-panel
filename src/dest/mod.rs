//! Destination contracts; billing and settings capabilities have separate interfaces.
pub mod builtins;
pub mod claude_code_hub;
pub mod models_dev;
pub mod registry;

use serde::{Deserialize, Serialize};

pub use crate::Result;

/// A credential resolver supplied by the host; destinations never read Codex files.
pub trait Credentials: Send + Sync {
    fn api_key(&self, profile: Option<&str>) -> Result<String>;
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SessionContext {
    pub session_id: String,
    pub credential_profile: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DestinationConfig {
    pub name: String,
    /// Fixed billing currency for session totals, or None when token quotes supply it.
    pub billing_currency: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PaymentInfo {
    pub payment_currency: String,
    /// Payment currency units per one billing currency unit.
    pub exchange_rate: f64,
}

impl PaymentInfo {
    pub fn validate(&self) -> Result<()> {
        if self.payment_currency.trim().is_empty()
            || !self.exchange_rate.is_finite()
            || self.exchange_rate <= 0.0
        {
            return Err("invalid payment conversion".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    /// Billed amount in the session currency or the enclosing token quote's currency.
    pub amount: f64,
    /// None when the source cannot report a reliable request count (e.g. token telemetry).
    pub requests: Option<u64>,
}

impl Stats {
    pub fn validate(&self) -> Result<()> {
        if !self.amount.is_finite() || self.amount < 0.0 {
            return Err("invalid billed amount".into());
        }
        Ok(())
    }
}

pub trait Destination: Send + Sync {
    /// Session billing uses this currency; token quotes carry their own currency.
    fn config(&self) -> DestinationConfig;
    fn pricing(&self) -> PricingInterface<'_>;

    fn settings(&self) -> Option<std::sync::Arc<dyn DestinationSettings>> {
        None
    }

    fn display(&self, _context: &DisplayContext) -> Option<std::sync::Arc<dyn DestinationDisplay>> {
        None
    }
}

pub struct DisplayContext {
    pub session: Option<SessionContext>,
    pub model: Option<String>,
}

/// Optional monitor content; the host allocates space without understanding provider state.
pub trait DestinationDisplay: Send + Sync {
    fn height(&self, width: u16) -> u16;
    fn render(&self, frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect);
}

/// The host supplies an area; the destination owns its widgets, layout and input.
pub trait DestinationSettings: Send + Sync {
    fn height(&self, width: u16) -> u16;
    fn render(&self, frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect);
    fn handle(&self, _event: &crossterm::event::Event) {}
    fn save(&self) -> Result<()> {
        Ok(())
    }
}

/// Exactly two capabilities; each variant requires its corresponding implementation.
pub enum PricingInterface<'a> {
    SessionTotals(&'a dyn SessionPricing),
    TokenPrices(&'a dyn TokenPricing),
}

pub trait SessionPricing: Send + Sync {
    /// Cumulative session bill and whether a rejected authentication was recovered internally.
    /// Transport, authentication, renewal and response decoding belong to the destination.
    fn session_totals(&self, context: &SessionContext) -> Result<(Stats, bool)>;
}

pub trait TokenPricing: Send + Sync {
    /// Effective rates and their currency per million tokens, for this exact model.
    /// Return an error for an unknown model; never silently use another model's rates.
    fn token_prices(&self, context: Option<&SessionContext>, model: &str) -> Result<TokenQuote>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct TokenQuote {
    pub currency: String,
    pub prices: TokenPrices,
}

/// One observed model response. Sequence numbers are local to the monitoring run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRequest {
    pub sequence: u64,
    pub session_id: String,
    pub credential_profile: Option<String>,
    pub model: Option<String>,
    pub usage: TokenUsage,
}

/// Cumulative telemetry. Cached/write input are subsets of input; reasoning is a subset of output.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_write_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
}

impl TokenUsage {
    pub fn delta_from(&self, previous: &Self) -> Result<Self> {
        let delta = Self {
            input_tokens: self
                .input_tokens
                .checked_sub(previous.input_tokens)
                .ok_or("input token counter decreased")?,
            cached_input_tokens: self
                .cached_input_tokens
                .checked_sub(previous.cached_input_tokens)
                .ok_or("cached token counter decreased")?,
            cache_write_input_tokens: self
                .cache_write_input_tokens
                .checked_sub(previous.cache_write_input_tokens)
                .ok_or("cache write counter decreased")?,
            output_tokens: self
                .output_tokens
                .checked_sub(previous.output_tokens)
                .ok_or("output token counter decreased")?,
            reasoning_output_tokens: self
                .reasoning_output_tokens
                .checked_sub(previous.reasoning_output_tokens)
                .ok_or("reasoning token counter decreased")?,
        };
        delta.validate()?;
        Ok(delta)
    }

    pub fn validate(&self) -> Result<()> {
        let cached = self
            .cached_input_tokens
            .checked_add(self.cache_write_input_tokens)
            .ok_or("input token overflow")?;
        if cached > self.input_tokens || self.reasoning_output_tokens > self.output_tokens {
            return Err("token subsets exceed total tokens".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TokenPrices {
    pub input: f64,
    pub cached_input: f64,
    pub cache_write_input: f64,
    pub output: f64,
    pub reasoning_output: f64,
}

impl TokenPrices {
    pub fn estimate(&self, usage: &TokenUsage) -> Result<f64> {
        usage.validate()?;
        for rate in [
            self.input,
            self.cached_input,
            self.cache_write_input,
            self.output,
            self.reasoning_output,
        ] {
            if !rate.is_finite() || rate < 0.0 {
                return Err("invalid token price".into());
            }
        }
        let input = usage.input_tokens - usage.cached_input_tokens - usage.cache_write_input_tokens;
        let output = usage.output_tokens - usage.reasoning_output_tokens;
        let amount = (input as f64 * self.input
            + usage.cached_input_tokens as f64 * self.cached_input
            + usage.cache_write_input_tokens as f64 * self.cache_write_input
            + output as f64 * self.output
            + usage.reasoning_output_tokens as f64 * self.reasoning_output)
            / 1_000_000.0;
        if !amount.is_finite() {
            return Err("token cost overflow".into());
        }
        Ok(amount)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_categories_are_not_double_billed_and_bad_data_is_rejected() {
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            cached_input_tokens: 200_000,
            cache_write_input_tokens: 100_000,
            output_tokens: 100_000,
            reasoning_output_tokens: 50_000,
        };
        let prices = TokenPrices {
            input: 2.0,
            cached_input: 0.5,
            cache_write_input: 3.0,
            output: 10.0,
            reasoning_output: 12.0,
        };
        assert!((prices.estimate(&usage).unwrap() - 2.9).abs() < 1e-10);
        assert_eq!(usage.delta_from(&usage).unwrap(), TokenUsage::default());
        assert!(TokenUsage::default().delta_from(&usage).is_err());
        assert!(
            prices
                .estimate(&TokenUsage {
                    cached_input_tokens: 1,
                    ..Default::default()
                })
                .is_err()
        );
        assert!(
            TokenPrices {
                input: f64::NAN,
                ..prices
            }
            .estimate(&usage)
            .is_err()
        );
    }
}
