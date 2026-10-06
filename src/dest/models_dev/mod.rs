//! Aggregated token pricing. Source protocol, configuration and presentation stay adapter-owned.
mod config;
mod pricing;
mod view;

use super::{
    Destination, DestinationConfig, DestinationDisplay, DestinationSettings, DisplayContext,
    PricingInterface, Result, SessionContext, TokenPricing, TokenQuote,
    registry::DestinationFactory,
};
use crate::source::{Retrieval, cache::Cache};
use config::Config;
use pricing::Catalog;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct State {
    model: String,
    quote: Option<TokenQuote>,
}

pub struct ModelsDev {
    name: String,
    config: Config,
    retrieval: Arc<dyn Retrieval>,
    cache: Cache<Arc<Catalog>>,
    state: Mutex<HashMap<(Option<SessionContext>, String), State>>,
}

impl ModelsDev {
    pub const TYPE: &'static str = "models_dev";

    pub fn parse_config(
        value: &toml::Value,
        complete: bool,
    ) -> Result<Option<Arc<dyn DestinationFactory>>> {
        config::parse(value, complete)
    }

    fn new(name: String, config: Config, retrieval: Arc<dyn Retrieval>) -> Self {
        let cache = Cache::new(config.cache);
        Self {
            name,
            config,
            retrieval,
            cache,
            state: Mutex::new(HashMap::new()),
        }
    }
}

impl Destination for ModelsDev {
    fn config(&self) -> DestinationConfig {
        DestinationConfig {
            name: self.name.clone(),
            billing_currency: Some("USD".into()),
        }
    }

    fn pricing(&self) -> PricingInterface<'_> {
        PricingInterface::TokenPrices(self)
    }

    fn settings(&self) -> Option<Arc<dyn DestinationSettings>> {
        Some(Arc::new(view::Settings(self.config.clone())))
    }

    fn display(&self, context: &DisplayContext) -> Option<Arc<dyn DestinationDisplay>> {
        let state = context
            .model
            .as_ref()
            .and_then(|model| {
                let states = self.state.lock().ok()?;
                states
                    .get(&(context.session.clone(), model.clone()))
                    .or_else(|| states.get(&(None, model.clone())))
                    .cloned()
            })
            .unwrap_or_default();
        Some(Arc::new(view::Display {
            state,
            model: context.model.clone(),
        }))
    }
}

impl TokenPricing for ModelsDev {
    fn token_prices(&self, context: Option<&SessionContext>, model: &str) -> Result<TokenQuote> {
        let result = self
            .cache
            .get(|| Catalog::decode(&self.retrieval.get()?, &self.config.provider_id).map(Arc::new))
            .and_then(|catalog| {
                let model = self
                    .config
                    .aliases
                    .get(model)
                    .map(String::as_str)
                    .unwrap_or(model);
                catalog.select(model)
            });
        let mut states = self
            .state
            .lock()
            .map_err(|_| "price status lock poisoned")?;
        let selection = result?;
        states.insert(
            (context.cloned(), model.to_owned()),
            State {
                model: selection.model,
                quote: Some(selection.quote.clone()),
            },
        );
        Ok(selection.quote)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn configured_http_catalog_is_cached_and_never_uses_destination_credentials() {
        use crate::dest::{Credentials, registry::CreateContext};
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;

        struct NoCredentials;
        impl Credentials for NoCredentials {
            fn api_key(&self, _: Option<&str>) -> Result<String> {
                panic!("independent catalog requests must not read destination credentials")
            }
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let source_url = format!("http://{}/catalog", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.starts_with("GET /catalog "));
            loop {
                line.clear();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                let header = line.to_ascii_lowercase();
                assert!(!header.starts_with("authorization:") && !header.starts_with("cookie:"));
            }
            let body = r#"{"example.provider":{"models":{"lab/model":{"name":"Model","cost":{"input":0.15,"cache_read":0.003,"output":0.6}}}}}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let config = format!("provider_id = 'example.provider'\nsource_url = '{source_url}'\nnote = 'Example note'\n[model_aliases]\napi-model = 'lab/model'").parse().unwrap();
        let factory = ModelsDev::parse_config(&config, true).unwrap().unwrap();
        assert_eq!(factory.warnings(), ["Example note"]);
        let adapter = factory
            .create(CreateContext {
                name: "Example".into(),
                credentials: Arc::new(NoCredentials),
            })
            .unwrap();
        assert_eq!(adapter.config().billing_currency.as_deref(), Some("USD"));
        let PricingInterface::TokenPrices(pricing) = adapter.pricing() else {
            panic!()
        };
        for profile in ["a", "b"] {
            let quote = pricing
                .token_prices(
                    Some(&SessionContext {
                        session_id: "same".into(),
                        credential_profile: Some(profile.into()),
                    }),
                    "api-model",
                )
                .unwrap();
            assert_eq!(quote.currency, "USD");
            assert_eq!(quote.prices.cached_input, 0.003);
        }
        server.join().unwrap();
    }
}
