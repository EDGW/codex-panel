//! Select destinations from host configuration and keep each destination's monitoring state.
use crate::AppResult;
use crate::config::LoadedConfig;
use crate::cost::{CostDisplay, CostStatus, Monitor, Observation};
use crate::credentials::CodexCredentials;
use crate::dest::registry::CreateContext;
use crate::exchange::{ConversionDisplay, Exchange};
use crate::session::Session;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default)]
pub struct BillingDisplay {
    pub session: CostDisplay,
    pub monitoring: CostDisplay,
    pub detail: String,
    pub detail_warning: bool,
    pub api_url: String,
    pub destination: Option<String>,
    pub billing_currency: Option<String>,
    pub conversion: ConversionDisplay,
    pub settings_ui: Option<Arc<dyn crate::dest::DestinationSettings>>,
}

struct DestinationMonitor {
    destination: Arc<dyn crate::dest::Destination>,
    settings_ui: Option<Arc<dyn crate::dest::DestinationSettings>>,
    costs: Monitor,
    exchange: Exchange,
}

pub struct Billing {
    config: LoadedConfig,
    credentials: Arc<CodexCredentials>,
    monitors: HashMap<(String, Option<String>), DestinationMonitor>,
    selected_profile: Option<Option<String>>,
    selection: Result<(String, Option<String>), String>,
    api_url: String,
}

impl Billing {
    pub fn new(config: LoadedConfig) -> AppResult<Self> {
        Ok(Self::with_credentials(
            config,
            Arc::new(CodexCredentials::discover()?),
        ))
    }

    fn with_credentials(config: LoadedConfig, credentials: Arc<CodexCredentials>) -> Self {
        Self {
            config,
            credentials,
            monitors: HashMap::new(),
            selected_profile: None,
            selection: Err("Connecting...".into()),
            api_url: String::new(),
        }
    }

    fn select(&mut self, profile: Option<&str>) -> Result<(String, Option<String>), String> {
        self.api_url.clear();
        let profile = self.credentials.profile(profile)?;
        let url = self.credentials.api_url(Some(&profile))?;
        self.api_url = url.clone();
        let id = self.config.mappings.destination_id(&url)?.to_owned();
        let key = (id.clone(), Some(profile));
        if !self.monitors.contains_key(&key) {
            let instance = self.config.instance(&id);
            let destination = instance.factory.create(CreateContext {
                credentials: self.credentials.clone(),
                name: instance.name.clone(),
            })?;
            self.monitors.insert(
                key.clone(),
                DestinationMonitor {
                    settings_ui: destination.settings(),
                    destination: destination.clone(),
                    costs: Monitor::new(destination.clone()),
                    exchange: Exchange::new(instance.conversion.clone())?,
                },
            );
        }
        Ok(key)
    }

    pub fn update(&mut self, session: &Session) -> BillingDisplay {
        if self.selected_profile.as_ref() != Some(&session.model_provider) {
            self.selected_profile = Some(session.model_provider.clone());
            self.selection = self.select(session.model_provider.as_deref());
        }
        let id = match &self.selection {
            Ok(key) => key,
            Err(error) => {
                return BillingDisplay {
                    session: CostDisplay {
                        status: CostStatus::Unavailable,
                        ..Default::default()
                    },
                    monitoring: CostDisplay {
                        status: CostStatus::Unavailable,
                        ..Default::default()
                    },
                    detail: error.clone(),
                    detail_warning: true,
                    api_url: self.api_url.clone(),
                    ..BillingDisplay::default()
                };
            }
        };
        let monitor = self
            .monitors
            .get_mut(id)
            .expect("selected destination was created");
        let observation = Observation {
            session_id: session.thread_id.clone(),
            credential_profile: id.1.clone(),
            model: session.model.clone(),
            usage: session.token_usage.clone(),
            revision: session.cost_revision,
        };
        let queried = monitor.costs.update(&observation);
        monitor.exchange.update(queried);
        let conversion = monitor.exchange.display();
        let detail_warning = session
            .error
            .as_ref()
            .is_some_and(|error| !error.is_empty())
            || !monitor.costs.detail().is_empty()
            || conversion.warning;
        BillingDisplay {
            api_url: self.api_url.clone(),
            billing_currency: Some(monitor.destination.config().billing_currency),
            conversion,
            destination: Some(format!(
                "{} [{} · {}]",
                monitor.destination.config().name,
                id.0,
                self.config.instance(&id.0).kind
            )),
            settings_ui: monitor.settings_ui.clone(),
            session: monitor
                .costs
                .session_display(monitor.exchange.payment.as_ref()),
            monitoring: monitor
                .costs
                .monitoring_display(monitor.exchange.payment.as_ref()),
            detail_warning,
            detail: [
                session.error.as_deref().unwrap_or(""),
                monitor.costs.detail(),
                &monitor.exchange.detail(),
            ]
            .into_iter()
            .filter(|detail| !detail.is_empty())
            .collect::<Vec<_>>()
            .join(" | "),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConfigPaths, load_with_registry};
    use crate::conversion::PaymentConversion;
    use crate::dest::{
        Destination, DestinationConfig, PaymentInfo, PricingInterface, SessionContext,
        SessionPricing, Stats,
        registry::{DestinationFactory, Registry},
    };
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    type Amounts = Arc<Mutex<HashMap<(String, String), f64>>>;
    struct TestFactory(Amounts);
    struct TestDestination {
        name: String,
        amounts: Amounts,
    }
    impl DestinationFactory for TestFactory {
        fn create(&self, context: CreateContext) -> Result<Arc<dyn Destination>, String> {
            Ok(Arc::new(TestDestination {
                name: context.name,
                amounts: self.0.clone(),
            }))
        }
    }
    impl Destination for TestDestination {
        fn config(&self) -> DestinationConfig {
            DestinationConfig {
                name: self.name.clone(),
                billing_currency: "USD".into(),
            }
        }
        fn pricing(&self) -> PricingInterface<'_> {
            PricingInterface::SessionTotals(self)
        }
    }
    impl SessionPricing for TestDestination {
        fn session_totals(&self, context: &SessionContext) -> Result<(Stats, bool), String> {
            let key = (
                self.name.clone(),
                context
                    .credential_profile
                    .clone()
                    .expect("effective profile"),
            );
            Ok((
                Stats {
                    amount: self.amounts.lock().unwrap()[&key],
                    requests: Some(1),
                },
                false,
            ))
        }
    }
    struct FailingConversion;
    impl PaymentConversion for FailingConversion {
        fn payment_info(&self) -> Result<PaymentInfo, String> {
            Err("price source unavailable".into())
        }
        fn initial_payment(&self) -> Result<Option<PaymentInfo>, String> {
            Ok(None)
        }
        fn cache_duration(&self) -> Duration {
            Duration::from_secs(300)
        }
    }

    fn settled(billing: &mut Billing, session: &Session, amount: f64) -> BillingDisplay {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let display = billing.update(session);
            if display
                .session
                .to_string()
                .starts_with(&format!("${amount:.8} USD"))
                && display.session.to_string().ends_with("Connected")
            {
                return display;
            }
            assert!(
                Instant::now() < deadline,
                "{} | {}",
                display.session,
                display.detail
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn monitoring_isolated_by_instance_and_effective_profile_survives_switches_and_conversion_failure()
     {
        let root =
            std::env::temp_dir().join(format!("ccp-billing-isolation-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("config.toml"), "model_provider='a1'\n[model_providers.a1]\nbase_url='https://a.example/v1'\n[model_providers.a2]\nbase_url='https://a.example/v1'\n[model_providers.b1]\nbase_url='https://b.example/v1'").unwrap();
        let defaults = root.join("destinations.toml");
        std::fs::write(&defaults, "version=1\n[[destinations]]\nid='a'\ntype='test'\nname='A'\napi_urls=['https://a.example/v1']\n[destinations.conversion]\ncurrency='EUR'\n[destinations.conversion.source]\ntype='value'\nvalue=0.5\n[[destinations]]\nid='b'\ntype='test'\nname='B'\napi_urls=['https://b.example/v1']").unwrap();
        let amounts: Amounts = Arc::new(Mutex::new(HashMap::from([
            (("A".into(), "a1".into()), 10.0),
            (("A".into(), "a2".into()), 50.0),
            (("B".into(), "b1".into()), 100.0),
        ])));
        let source = amounts.clone();
        let mut registry = Registry::new();
        registry
            .register("test", move |value, complete| {
                assert!(value.as_table().unwrap().is_empty());
                Ok(complete
                    .then(|| Arc::new(TestFactory(source.clone())) as Arc<dyn DestinationFactory>))
            })
            .unwrap();
        let mut config = load_with_registry(
            &ConfigPaths {
                defaults,
                user: None,
            },
            &registry,
        )
        .unwrap();
        config
            .instances
            .iter_mut()
            .find(|i| i.id == "b")
            .unwrap()
            .conversion = Some(Arc::new(FailingConversion));
        let mut billing =
            Billing::with_credentials(config, Arc::new(CodexCredentials::at(root.clone())));
        let mut session = Session {
            thread_id: Some("shared-session-id".into()),
            ..Default::default()
        };
        let initial = settled(&mut billing, &session, 10.0);
        assert!(
            initial
                .monitoring
                .to_string()
                .starts_with("$0.00000000 USD (0.000000 EUR)")
        );
        assert!(
            initial
                .destination
                .as_ref()
                .unwrap()
                .contains("A [a · test]")
        );
        let mut observe = |profile: &str, name: &str, amount: f64, increment: f64| {
            amounts
                .lock()
                .unwrap()
                .insert((name.into(), profile.into()), amount);
            session.model_provider = Some(profile.into());
            session.cost_revision += 1;
            let display = settled(&mut billing, &session, amount);
            assert!(
                display
                    .monitoring
                    .to_string()
                    .starts_with(&format!("${increment:.8} USD")),
                "{}",
                display.monitoring
            );
            display
        };
        // Explicitly selecting the configured default reuses the effective-profile baseline.
        observe("a1", "A", 12.0, 2.0);
        let b = observe("b1", "B", 100.0, 0.0);
        assert!(!b.session.to_string().contains("EUR"));
        observe("b1", "B", 105.0, 5.0);
        observe("a2", "A", 50.0, 0.0);
        observe("a2", "A", 51.0, 1.0);
        let returning = observe("a1", "A", 13.0, 3.0);
        assert!(returning.session.to_string().contains("(6.500000 EUR)"));
        observe("b1", "B", 105.0, 5.0);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let display = billing.update(&session);
            if display.detail.contains("Payment conversion unavailable") {
                assert!(display.session.to_string().starts_with("$105.00000000 USD"));
                assert!(
                    display
                        .monitoring
                        .to_string()
                        .starts_with("$5.00000000 USD")
                );
                assert!(display.conversion.configured);
                assert!(display.detail_warning);
                assert!(display.conversion.warning);
                assert!(display.conversion.payment.is_none());
                assert!(
                    display
                        .conversion
                        .status
                        .contains("Payment conversion unavailable")
                );
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(billing);
        std::fs::remove_dir_all(root).unwrap();
    }
}
