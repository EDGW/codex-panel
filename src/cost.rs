mod requests;
mod worker;

use crate::dest::{
    Destination, DestinationConfig, PricingInterface, Result, SessionContext, Stats, TokenQuote,
    TokenRequest, TokenUsage,
};
use requests::RequestQueue;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

fn retry_delay(failures: u32) -> Duration {
    Duration::from_secs(1u64 << failures.min(6)).min(Duration::from_secs(60))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CostStatus {
    #[default]
    Connecting,
    Connected,
    Reconnected,
    Reconnecting,
    Unavailable,
    NotAvailable,
}

/// A billed amount in its original currency, before payment conversion.
#[derive(Clone, Debug, PartialEq)]
pub struct Money {
    pub amount: f64,
    pub currency: String,
}

/// None denotes unavailable amounts; an empty list denotes zero before a currency is known.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CostSnapshot {
    pub amounts: Option<Vec<Money>>,
    pub requests: Option<u64>,
    pub estimated: bool,
    pub status: CostStatus,
}

/// Host telemetry translated into destination-independent data.
#[derive(Clone, Debug, Default)]
pub struct Observation {
    pub session_id: Option<String>,
    pub credential_profile: Option<String>,
    pub model: Option<String>,
    pub usage: Option<TokenUsage>,
    pub requests: Vec<TokenRequest>,
    pub revision: u64,
}

impl Observation {
    fn context(&self) -> Option<SessionContext> {
        Some(SessionContext {
            session_id: self.session_id.clone()?,
            credential_profile: self.credential_profile.clone(),
        })
    }
}

#[derive(Clone)]
struct Job {
    generation: u64,
    target: Option<SessionContext>,
    model: Option<String>,
    request: Option<TokenRequest>,
}

enum Reading {
    Session(Stats, bool),
    Quote(TokenQuote),
    Tokens { currency: String, amount: f64 },
}

struct Reply {
    job: Job,
    result: Result<Reading>,
}

#[derive(Default)]
struct SessionAccounting {
    revision: u64,
    model: Option<String>,
    usage: Option<TokenUsage>,
    stats: Option<Stats>,
    accounted: HashMap<SessionContext, Stats>,
    monitoring: Stats,
}

impl SessionAccounting {
    fn record(&mut self, target: SessionContext, stats: Stats) {
        let previous = self
            .accounted
            .entry(target)
            .or_insert_with(|| stats.clone());
        self.monitoring.amount += (stats.amount - previous.amount).max(0.0);
        self.monitoring.requests = match (stats.requests, previous.requests) {
            (Some(current), Some(last)) => Some(
                self.monitoring
                    .requests
                    .unwrap_or(0)
                    .saturating_add(current.saturating_sub(last)),
            ),
            _ => None,
        };
        previous.amount = previous.amount.max(stats.amount);
        previous.requests = match (previous.requests, stats.requests) {
            (Some(a), Some(b)) => Some(a.max(b)),
            _ => None,
        };
        self.stats = Some(stats);
    }
}

#[derive(Default)]
struct TokenAccounting {
    model: Option<String>,
    requests: RequestQueue,
    amounts: BTreeMap<String, Stats>,
}

impl TokenAccounting {
    fn record(&mut self, currency: String, amount: f64, request: bool) {
        let sum = self.amounts.entry(currency).or_insert(Stats {
            amount: 0.0,
            requests: Some(0),
        });
        sum.amount += amount;
        if request {
            sum.requests = Some(sum.requests.unwrap_or(0).saturating_add(1));
        }
    }
}

enum Accounting {
    Session(SessionAccounting),
    Tokens(TokenAccounting),
}

pub struct Monitor {
    config: DestinationConfig,
    accounting: Accounting,
    jobs: mpsc::Sender<Job>,
    replies: mpsc::Receiver<Reply>,
    target: Option<SessionContext>,
    generation: u64,
    pending: bool,
    next: Option<Instant>,
    dirty: bool,
    interrupted: bool,
    reconnected: bool,
    failures: u32,
    error: Option<String>,
}

impl Monitor {
    pub fn new(destination: Arc<dyn Destination>) -> Self {
        let config = destination.config();
        let accounting = match destination.pricing() {
            PricingInterface::SessionTotals(_) => Accounting::Session(SessionAccounting::default()),
            PricingInterface::TokenPrices(_) => Accounting::Tokens(TokenAccounting::default()),
        };
        let (jobs, replies) = worker::spawn(destination);
        Self {
            config,
            accounting,
            jobs,
            replies,
            target: None,
            generation: 0,
            pending: false,
            next: None,
            dirty: false,
            interrupted: false,
            reconnected: false,
            failures: 0,
            error: None,
        }
    }

    fn apply(&mut self, reply: Reply) {
        let current = reply.job.generation == self.generation && self.target == reply.job.target;
        match &mut self.accounting {
            Accounting::Tokens(tokens) => {
                self.pending = false;
                if reply.job.request.is_none() && (reply.job.model != tokens.model || !current) {
                    return;
                }
                if let Some(request) = &reply.job.request {
                    match &reply.result {
                        Ok(_) => tokens.requests.complete(request.sequence),
                        Err(error) => {
                            tokens.requests.fail(request, error);
                            self.interrupted = true;
                            return;
                        }
                    }
                }
            }
            Accounting::Session(_) => {
                if !current {
                    return;
                }
                self.pending = false;
            }
        }
        match reply.result {
            Ok(reading) => {
                let recovered = match (&mut self.accounting, reading) {
                    (Accounting::Session(session), Reading::Session(stats, recovered)) => {
                        session.record(
                            reply.job.target.expect("session totals have a target"),
                            stats,
                        );
                        recovered
                    }
                    (Accounting::Tokens(tokens), Reading::Quote(quote)) => {
                        tokens.record(quote.currency, 0.0, false);
                        false
                    }
                    (Accounting::Tokens(tokens), Reading::Tokens { currency, amount }) => {
                        tokens.record(currency, amount, true);
                        false
                    }
                    _ => unreachable!("worker replies match the pricing capability"),
                };
                self.reconnected |= self.interrupted || recovered;
                if reply.job.request.is_none() {
                    self.error = None;
                    self.failures = 0;
                    self.next = None;
                }
            }
            Err(error) => {
                self.error = Some(error);
                self.interrupted = true;
                self.failures = self.failures.saturating_add(1);
                self.next = Some(Instant::now() + retry_delay(self.failures));
            }
        }
    }

    /// Returns true when a billing/price query is submitted, so payment conversion can refresh.
    pub fn update(&mut self, observation: &Observation) -> bool {
        let target = observation.context();
        let changed = self.target != target;
        if changed {
            self.target = target;
            self.generation += 1;
        }
        match &mut self.accounting {
            Accounting::Tokens(tokens) => {
                tokens
                    .requests
                    .observe(&observation.requests, self.generation);
                if changed || tokens.model != observation.model {
                    self.dirty = true;
                    // A new selection should prefetch immediately, independent of
                    // backoff for a previous model's quote.
                    self.error = None;
                    self.failures = 0;
                    self.next = None;
                }
                tokens.model = observation.model.clone();
            }
            Accounting::Session(session) => {
                if changed {
                    self.pending = false;
                    session.stats = None;
                    self.error = None;
                    self.failures = 0;
                    self.next = None;
                    session.revision = observation.revision;
                    self.dirty = self.target.is_some();
                }
                if self.target.is_some()
                    && (session.model != observation.model
                        || session.revision != observation.revision
                        || session.usage != observation.usage)
                {
                    self.dirty = true;
                }
                session.model = observation.model.clone();
                session.revision = observation.revision;
                session.usage = observation.usage.clone();
            }
        }
        while let Ok(reply) = self.replies.try_recv() {
            self.apply(reply);
        }
        let now = Instant::now();
        let (request, model, can_refresh) = match &self.accounting {
            Accounting::Tokens(tokens) => (
                tokens.requests.ready(now),
                tokens.model.clone(),
                tokens.model.is_some(),
            ),
            Accounting::Session(session) => (None, session.model.clone(), self.target.is_some()),
        };
        let refresh_ready = can_refresh
            && self.next.is_none_or(|next| now >= next)
            && (self.dirty || self.next.is_some());
        if !self.pending && (request.is_some() || refresh_ready) {
            let job = request.cloned().unwrap_or(Job {
                generation: self.generation,
                target: self.target.clone(),
                model,
                request: None,
            });
            let prefetch = job.request.is_none();
            if self.jobs.send(job).is_ok() {
                self.pending = true;
                if prefetch {
                    self.dirty = false;
                    self.next = None;
                }
                return true;
            }
            self.error = Some("cost worker stopped".into());
            self.interrupted = true;
            self.next = Some(Instant::now() + Duration::from_secs(60));
        }
        false
    }

    fn status(&self) -> CostStatus {
        if !self.detail().is_empty() {
            CostStatus::Reconnecting
        } else if matches!(&self.accounting, Accounting::Tokens(tokens) if tokens.model.is_none()) {
            CostStatus::Connected
        } else if matches!(&self.accounting, Accounting::Session(session) if session.stats.is_none())
        {
            CostStatus::Connecting
        } else if self.reconnected {
            CostStatus::Reconnected
        } else {
            CostStatus::Connected
        }
    }

    fn session_snapshot(&self, stats: Option<&Stats>) -> CostSnapshot {
        let Some(stats) = stats else {
            return CostSnapshot {
                status: self.status(),
                ..Default::default()
            };
        };
        let Some(currency) = &self.config.billing_currency else {
            return CostSnapshot {
                status: CostStatus::Unavailable,
                ..Default::default()
            };
        };
        CostSnapshot {
            amounts: Some(vec![Money {
                amount: stats.amount,
                currency: currency.clone(),
            }]),
            requests: stats.requests,
            estimated: false,
            status: self.status(),
        }
    }

    pub fn session_cost(&self) -> CostSnapshot {
        match &self.accounting {
            Accounting::Session(session) => self.session_snapshot(session.stats.as_ref()),
            Accounting::Tokens(_) => CostSnapshot {
                status: CostStatus::NotAvailable,
                estimated: true,
                ..Default::default()
            },
        }
    }

    pub fn monitoring_cost(&self) -> CostSnapshot {
        let tokens = match &self.accounting {
            Accounting::Session(session) => {
                return self.session_snapshot(
                    (!session.accounted.is_empty()).then_some(&session.monitoring),
                );
            }
            Accounting::Tokens(tokens) => tokens,
        };
        let mut amounts: Vec<_> = tokens
            .amounts
            .iter()
            .map(|(currency, stats)| Money {
                amount: stats.amount,
                currency: currency.clone(),
            })
            .collect();
        if amounts.is_empty()
            && let Some(currency) = &self.config.billing_currency
        {
            amounts.push(Money {
                amount: 0.0,
                currency: currency.clone(),
            });
        }
        CostSnapshot {
            amounts: Some(amounts),
            requests: Some(tokens.amounts.values().fold(0u64, |total, stats| {
                total.saturating_add(stats.requests.unwrap_or(0))
            })),
            estimated: true,
            status: self.status(),
        }
    }

    pub fn billing_currencies(&self) -> Vec<String> {
        match &self.accounting {
            Accounting::Tokens(tokens) => tokens.amounts.keys().cloned().collect(),
            Accounting::Session(_) => self.config.billing_currency.iter().cloned().collect(),
        }
    }

    pub fn detail(&self) -> &str {
        self.error
            .as_deref()
            .or_else(|| match &self.accounting {
                Accounting::Tokens(tokens) => tokens.requests.error(),
                Accounting::Session(_) => None,
            })
            .unwrap_or("")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::dest::{SessionPricing, TokenPrices, TokenPricing, TokenQuote};

    fn retry_request(monitor: &mut Monitor, sequence: u64) {
        let Accounting::Tokens(tokens) = &mut monitor.accounting else {
            panic!()
        };
        tokens.requests.retry_now(sequence);
    }

    fn assert_cost(
        cost: CostSnapshot,
        amounts: &[(&str, f64)],
        requests: Option<u64>,
        estimated: bool,
        status: CostStatus,
    ) {
        assert_eq!(
            cost,
            CostSnapshot {
                amounts: Some(
                    amounts
                        .iter()
                        .map(|(currency, amount)| Money {
                            currency: (*currency).into(),
                            amount: *amount
                        })
                        .collect()
                ),
                requests,
                estimated,
                status,
            }
        );
    }

    struct FakeSessionDestination;
    impl Destination for FakeSessionDestination {
        fn config(&self) -> DestinationConfig {
            DestinationConfig {
                name: "Fake".into(),
                billing_currency: Some("USD".into()),
            }
        }
        fn pricing(&self) -> PricingInterface<'_> {
            PricingInterface::SessionTotals(self)
        }
    }
    impl SessionPricing for FakeSessionDestination {
        fn session_totals(&self, _: &SessionContext) -> Result<(Stats, bool)> {
            Ok((
                Stats {
                    amount: 0.0,
                    requests: Some(0),
                },
                false,
            ))
        }
    }
    fn test_monitor() -> Monitor {
        Monitor::new(Arc::new(FakeSessionDestination))
    }

    #[derive(Default)]
    struct FakeTokenDestination {
        fail_next: std::sync::atomic::AtomicBool,
    }
    impl Destination for FakeTokenDestination {
        fn config(&self) -> DestinationConfig {
            DestinationConfig {
                name: "Token fake".into(),
                billing_currency: Some("CNY".into()),
            }
        }
        fn pricing(&self) -> PricingInterface<'_> {
            PricingInterface::TokenPrices(self)
        }
    }
    impl TokenPricing for FakeTokenDestination {
        fn token_prices(&self, _: Option<&SessionContext>, model: &str) -> Result<TokenQuote> {
            if self
                .fail_next
                .swap(false, std::sync::atomic::Ordering::Relaxed)
            {
                return Err("temporary price source failure".into());
            }
            if !matches!(model, "test-model" | "cheap-model" | "other-currency") {
                return Err("unknown model".into());
            }
            let multiplier = if model == "cheap-model" { 0.5 } else { 1.0 };
            Ok(TokenQuote {
                currency: if model == "other-currency" {
                    "USD"
                } else {
                    "CNY"
                }
                .into(),
                prices: TokenPrices {
                    input: 2.0 * multiplier,
                    cached_input: 0.5 * multiplier,
                    cache_write_input: 2.0 * multiplier,
                    output: 10.0 * multiplier,
                    reasoning_output: 10.0 * multiplier,
                },
            })
        }
    }

    #[test]
    fn estimated_monitor_prices_each_request_once_and_retries_its_original_model() {
        let destination = Arc::new(FakeTokenDestination::default());
        let mut monitor = Monitor::new(destination.clone());
        let mut observation = Observation::default();
        assert!(!monitor.update(&observation));
        assert_eq!(monitor.session_cost().status, CostStatus::NotAvailable);
        assert_cost(
            monitor.monitoring_cost(),
            &[("CNY", 0.0)],
            Some(0),
            true,
            CostStatus::Connected,
        );
        let settle = |monitor: &mut Monitor, observation: &Observation| {
            assert!(monitor.update(observation));
            let reply = monitor
                .replies
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            monitor.apply(reply);
        };
        observation.model = Some("test-model".into());
        settle(&mut monitor, &observation);
        let request = |sequence, model: &str| TokenRequest {
            sequence,
            session_id: "a".into(),
            credential_profile: None,
            model: Some(model.into()),
            usage: TokenUsage {
                input_tokens: 1_000_000,
                ..Default::default()
            },
        };
        // Both responses arrive before the next UI refresh; neither may be merged or repriced.
        observation.requests = vec![request(1, "test-model"), request(2, "cheap-model")];
        settle(&mut monitor, &observation);
        settle(&mut monitor, &observation);
        assert_cost(
            monitor.monitoring_cost(),
            &[("CNY", 3.0)],
            Some(2),
            true,
            CostStatus::Connected,
        );
        assert!(!monitor.update(&observation));
        destination
            .fail_next
            .store(true, std::sync::atomic::Ordering::Relaxed);
        observation.requests.push(request(3, "test-model"));
        settle(&mut monitor, &observation);
        assert_eq!(monitor.monitoring_cost().amounts.unwrap()[0].amount, 3.0);
        assert_eq!(monitor.monitoring_cost().requests, Some(2));
        observation.model = Some("cheap-model".into());
        observation.session_id = Some("other-session".into());
        // Price prefetch for the new selection is independent of the failed response.
        settle(&mut monitor, &observation);
        assert!(!monitor.detail().is_empty());

        // Retry the original response after a temporary source failure.
        retry_request(&mut monitor, 3);
        settle(&mut monitor, &observation);
        assert_cost(
            monitor.monitoring_cost(),
            &[("CNY", 5.0)],
            Some(3),
            true,
            CostStatus::Reconnected,
        );
        assert_eq!(monitor.session_cost().status, CostStatus::NotAvailable);
        assert!(!monitor.update(&observation));
        assert_eq!(monitor.monitoring_cost().requests, Some(3));
        observation.requests.push(request(4, "other-currency"));
        settle(&mut monitor, &observation);
        assert_cost(
            monitor.monitoring_cost(),
            &[("CNY", 5.0), ("USD", 2.0)],
            Some(4),
            true,
            CostStatus::Reconnected,
        );
    }

    #[test]
    fn failed_responses_do_not_block_other_models_or_prefetch_and_recover_once() {
        let destination = Arc::new(FakeTokenDestination::default());
        let mut monitor = Monitor::new(destination.clone());
        let request = |sequence, model| TokenRequest {
            sequence,
            session_id: "chat".into(),
            credential_profile: None,
            model,
            usage: TokenUsage {
                input_tokens: 1_000_000,
                ..Default::default()
            },
        };
        let mut observation = Observation {
            session_id: Some("chat".into()),
            model: Some("cheap-model".into()),
            requests: vec![
                request(1, Some("unknown".into())),
                request(2, None),
                request(3, Some("test-model".into())),
            ],
            ..Default::default()
        };
        let settle = |monitor: &mut Monitor, observation: &Observation| {
            assert!(monitor.update(observation));
            let reply = monitor
                .replies
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            let model = reply.job.model.clone();
            monitor.apply(reply);
            model
        };
        assert_eq!(
            settle(&mut monitor, &observation).as_deref(),
            Some("unknown")
        );
        assert_eq!(settle(&mut monitor, &observation), None);
        assert_eq!(
            settle(&mut monitor, &observation).as_deref(),
            Some("test-model")
        );
        assert_eq!(
            settle(&mut monitor, &observation).as_deref(),
            Some("cheap-model")
        );
        let costs = monitor.monitoring_cost();
        assert_eq!(costs.requests, Some(1));
        assert_eq!(costs.status, CostStatus::Reconnecting);
        assert!(monitor.detail().contains("unknown model"));
        assert!(!monitor.update(&observation));
        // A transient failure must preserve its original request and stay visible
        // even when subsequent responses are successfully priced.
        destination
            .fail_next
            .store(true, std::sync::atomic::Ordering::Relaxed);
        observation
            .requests
            .push(request(4, Some("test-model".into())));
        settle(&mut monitor, &observation);
        observation
            .requests
            .push(request(5, Some("cheap-model".into())));
        settle(&mut monitor, &observation);
        assert_eq!(monitor.monitoring_cost().requests, Some(2));
        retry_request(&mut monitor, 4);
        settle(&mut monitor, &observation);
        assert_eq!(monitor.monitoring_cost().requests, Some(3));
        assert_eq!(monitor.monitoring_cost().amounts.unwrap()[0].amount, 5.0);
        assert!(!monitor.update(&observation));
        assert!(monitor.detail().contains("unknown model"));
    }

    #[test]
    fn model_switch_prefetches_immediately_and_ignores_the_previous_quote() {
        let mut monitor = Monitor::new(Arc::new(FakeTokenDestination::default()));
        let mut observation = Observation {
            model: Some("unknown".into()),
            ..Default::default()
        };
        assert!(monitor.update(&observation));
        let failed_quote = monitor
            .replies
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        monitor.apply(failed_quote);
        assert!(monitor.next.is_some());
        observation.model = Some("test-model".into());
        assert!(
            monitor.update(&observation),
            "old model backoff must not delay the new quote"
        );
        let previous_quote = monitor
            .replies
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        observation.model = Some("other-currency".into());
        assert!(!monitor.update(&observation));
        monitor.apply(previous_quote);
        assert!(
            monitor.billing_currencies().is_empty(),
            "obsolete quote must not add a currency"
        );
        assert!(monitor.update(&observation));
        let current_quote = monitor
            .replies
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        monitor.apply(current_quote);
        assert_eq!(monitor.billing_currencies(), ["USD"]);
        assert_eq!(monitor.monitoring_cost().requests, Some(0));
        assert!(monitor.detail().is_empty());
    }

    #[test]
    fn monitoring_counts_only_increases_after_each_baseline_across_session_switches() {
        let mut monitor = test_monitor();
        let (sender, receiver) = mpsc::channel();
        monitor.jobs = sender;
        let mut observe = |session_id: &str, usd, requests| {
            let session = Observation {
                session_id: Some(session_id.into()),
                revision: match &monitor.accounting {
                    Accounting::Session(session) => session.revision + 1,
                    _ => unreachable!(),
                },
                ..Default::default()
            };
            monitor.update(&session);
            let job = receiver.try_recv().unwrap();
            monitor.apply(Reply {
                job,
                result: Ok(Reading::Session(
                    Stats {
                        amount: usd,
                        requests: Some(requests),
                    },
                    false,
                )),
            });
        };
        observe("a", 10.0, 100);
        observe("a", 11.0, 102);
        observe("a", 11.0, 102); // Duplicate snapshot.
        observe("a", 10.5, 101); // Delayed/older relay snapshot.
        observe("a", 12.0, 103);
        observe("b", 20.0, 200); // A different historical session gets its own baseline.
        observe("b", 20.5, 201);
        observe("a", 13.0, 105); // Returning does not reset a's baseline.
        assert_cost(
            monitor.session_cost(),
            &[("USD", 13.0)],
            Some(105),
            false,
            CostStatus::Connected,
        );
        assert_cost(
            monitor.monitoring_cost(),
            &[("USD", 3.5)],
            Some(6),
            false,
            CostStatus::Connected,
        );
        let job = Job {
            generation: monitor.generation,
            model: None,
            request: None,
            target: monitor.target.clone(),
        };
        monitor.apply(Reply {
            job,
            result: Err("network error".into()),
        });
        assert_eq!(monitor.monitoring_cost().status, CostStatus::Reconnecting);
        assert_eq!(monitor.monitoring_cost().requests, Some(6));
    }

    #[test]
    fn idle_costs_do_not_poll_and_events_during_queries_are_not_lost() {
        let mut m = test_monitor();
        let (tx, rx) = mpsc::channel();
        m.jobs = tx;
        let mut session = Observation {
            session_id: Some("chat".into()),
            ..Default::default()
        };
        m.update(&session);
        let job = rx.try_recv().unwrap();
        assert_eq!(m.session_cost().status, CostStatus::Connecting);
        m.apply(Reply {
            job,
            result: Ok(Reading::Session(
                Stats {
                    amount: 1.0,
                    requests: Some(1),
                },
                false,
            )),
        });
        assert_cost(
            m.session_cost(),
            &[("USD", 1.0)],
            Some(1),
            false,
            CostStatus::Connected,
        );
        m.update(&session);
        assert!(rx.try_recv().is_err());
        assert!(m.next.is_none());
        session.revision += 1;
        m.update(&session);
        let job = rx.try_recv().unwrap();
        session.revision += 1;
        m.update(&session);
        assert!(rx.try_recv().is_err());
        m.apply(Reply {
            job,
            result: Ok(Reading::Session(
                Stats {
                    amount: 2.0,
                    requests: Some(2),
                },
                false,
            )),
        });
        m.update(&session);
        assert!(
            rx.try_recv().is_ok(),
            "in-flight event must trigger a follow-up query"
        );
    }

    #[test]
    fn connection_history_survives_successful_refreshes() {
        let mut m = test_monitor();
        m.target = Some(SessionContext {
            session_id: "chat".into(),
            credential_profile: None,
        });
        let job = || Job {
            generation: 0,
            model: None,
            request: None,
            target: Some(SessionContext {
                session_id: "chat".into(),
                credential_profile: None,
            }),
        };
        m.apply(Reply {
            job: job(),
            result: Ok(Reading::Session(
                Stats {
                    amount: 1.0,
                    requests: Some(1),
                },
                false,
            )),
        });
        assert_eq!(m.session_cost().status, CostStatus::Connected);
        m.apply(Reply {
            job: job(),
            result: Err("network error".into()),
        });
        assert_cost(
            m.session_cost(),
            &[("USD", 1.0)],
            Some(1),
            false,
            CostStatus::Reconnecting,
        );
        assert!(m.next.is_some());
        m.apply(Reply {
            job: job(),
            result: Ok(Reading::Session(
                Stats {
                    amount: 2.0,
                    requests: Some(2),
                },
                false,
            )),
        });
        assert_eq!(m.session_cost().status, CostStatus::Reconnected);
        m.apply(Reply {
            job: job(),
            result: Ok(Reading::Session(
                Stats {
                    amount: 3.0,
                    requests: Some(3),
                },
                false,
            )),
        });
        assert_eq!(m.session_cost().status, CostStatus::Reconnected);
        let mut m = test_monitor();
        m.target = Some(SessionContext {
            session_id: "chat".into(),
            credential_profile: None,
        });
        m.apply(Reply {
            job: job(),
            result: Ok(Reading::Session(
                Stats {
                    amount: 1.0,
                    requests: Some(1),
                },
                true,
            )),
        });
        assert!(
            m.session_cost().status == CostStatus::Reconnected,
            "401 renewal must record interruption even if retry succeeds"
        );
    }

    #[test]
    fn stale_result_is_ignored_after_switching_sessions() {
        let mut monitor = test_monitor();
        monitor.target = Some(SessionContext {
            session_id: "new".into(),
            credential_profile: None,
        });
        monitor.generation = 2;
        monitor.pending = true;
        monitor.apply(Reply {
            job: Job {
                generation: 1,
                model: None,
                request: None,
                target: Some(SessionContext {
                    session_id: "old".into(),
                    credential_profile: None,
                }),
            },
            result: Ok(Reading::Session(
                Stats {
                    amount: 99.0,
                    requests: Some(1),
                },
                false,
            )),
        });
        assert!(monitor.session_cost().amounts.is_none());
        assert!(monitor.pending);
        // Returning to the same ID must still reject results from the previous visit.
        monitor.target = Some(SessionContext {
            session_id: "old".into(),
            credential_profile: None,
        });
        monitor.apply(Reply {
            job: Job {
                generation: 1,
                model: None,
                request: None,
                target: monitor.target.clone(),
            },
            result: Ok(Reading::Session(
                Stats {
                    amount: 99.0,
                    requests: Some(1),
                },
                false,
            )),
        });
        assert!(monitor.session_cost().amounts.is_none());
    }
}
