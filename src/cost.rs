use crate::dest::{
    Destination, DestinationConfig, PaymentInfo, PricingInterface, Result, SessionContext, Stats,
    TokenUsage,
};
use std::collections::HashMap;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CostStatus {
    #[default]
    Connecting,
    Connected,
    Reconnected,
    Reconnecting,
    Unavailable,
}

impl CostStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Connecting => "Connecting...",
            Self::Connected => "Connected",
            Self::Reconnected => "Reconnected",
            Self::Reconnecting => "Reconnecting...",
            Self::Unavailable => "Unavailable",
        }
    }

    pub fn is_warning(self) -> bool {
        matches!(self, Self::Reconnecting | Self::Unavailable)
    }
}

/// Separate primary amounts from ancillary counts and connection state.
#[derive(Clone, Debug, Default)]
pub struct CostDisplay {
    pub amount: Option<String>,
    pub requests: Option<u64>,
    pub estimated: bool,
    pub status: CostStatus,
}

impl std::fmt::Display for CostDisplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(amount) = &self.amount {
            write!(f, "{amount}")?;
            if let Some(requests) = self.requests {
                write!(f, " | {requests} Requests")?;
            }
            if self.estimated {
                write!(f, " · Estimated")?;
            }
            write!(f, " · ")?;
        }
        f.write_str(self.status.label())
    }
}

/// Host telemetry translated into destination-independent data.
#[derive(Clone, Debug, Default)]
pub struct Observation {
    pub session_id: Option<String>,
    pub credential_profile: Option<String>,
    pub model: Option<String>,
    pub usage: Option<TokenUsage>,
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

struct Job {
    generation: u64,
    target: SessionContext,
    model: Option<String>,
    usage: Option<TokenUsage>,
}

enum Reading {
    Session(Stats, bool),
    Tokens { session: Stats, increment: f64 },
}

struct Reply {
    job: Job,
    result: Result<Reading>,
}

/// Worker-side token ledger: calculate every accepted increment before a target can change.
/// This preserves increments when the UI discards a reply from a previous session selection.
#[derive(Default)]
struct TokenLedger {
    accounts: HashMap<SessionContext, (TokenUsage, f64)>,
}

impl TokenLedger {
    fn query(&mut self, destination: &dyn Destination, job: &Job) -> Result<Reading> {
        let PricingInterface::TokenPrices(pricing) = destination.pricing() else {
            return Err("destination has no token pricing interface".into());
        };
        let model = job
            .model
            .as_deref()
            .ok_or("model unavailable for token pricing")?;
        let total = job.usage.as_ref().ok_or("token usage unavailable")?;
        total.validate()?;
        let previous = self.accounts.get(&job.target);
        let delta = if let Some((previous, _)) = previous {
            total.delta_from(previous)?
        } else {
            TokenUsage::default()
        };
        let prices = pricing.token_prices(&job.target, model)?;
        let increment = prices.estimate(&delta)?;
        // Estimate the initial history with the current model, then retain priced increments.
        // Changing the model or rate does not reprice already observed history.
        let amount = if let Some((_, amount)) = previous {
            amount + increment
        } else {
            prices.estimate(total)?
        };
        let session = Stats {
            amount,
            requests: None,
        };
        session.validate()?;
        self.accounts
            .insert(job.target.clone(), (total.clone(), amount));
        Ok(Reading::Tokens { session, increment })
    }
}

pub struct Monitor {
    config: DestinationConfig,
    local: bool,
    jobs: mpsc::Sender<Job>,
    replies: mpsc::Receiver<Reply>,
    target: Option<SessionContext>,
    generation: u64,
    pending: bool,
    next: Option<Instant>,
    revision: u64,
    model: Option<String>,
    usage: Option<TokenUsage>,
    dirty: bool,
    interrupted: bool,
    reconnected: bool,
    failures: u32,
    stats: Option<Stats>,
    accounted: HashMap<SessionContext, Stats>,
    monitoring: Stats,
    local_observed: bool,
    error: Option<String>,
}

impl Monitor {
    pub fn new(destination: Arc<dyn Destination>) -> Self {
        let config = destination.config();
        let local = matches!(destination.pricing(), PricingInterface::TokenPrices(_));
        let (jobs, receiver) = mpsc::channel::<Job>();
        let (sender, replies) = mpsc::channel();
        thread::spawn(move || {
            let mut ledger = TokenLedger::default();
            while let Ok(mut job) = receiver.recv() {
                // Only cumulative session reads can safely skip obsolete queued jobs.
                if !local {
                    while let Ok(latest) = receiver.try_recv() {
                        job = latest;
                    }
                }
                let result = match destination.pricing() {
                    PricingInterface::SessionTotals(pricing) => pricing
                        .session_totals(&job.target)
                        .and_then(|(stats, recovered)| {
                            stats.validate()?;
                            Ok(Reading::Session(stats, recovered))
                        }),
                    PricingInterface::TokenPrices(_) => ledger.query(destination.as_ref(), &job),
                };
                if sender.send(Reply { job, result }).is_err() {
                    break;
                }
            }
        });
        Self {
            config,
            local,
            jobs,
            replies,
            target: None,
            generation: 0,
            pending: false,
            next: None,
            revision: 0,
            model: None,
            usage: None,
            dirty: false,
            interrupted: false,
            reconnected: false,
            failures: 0,
            stats: None,
            accounted: HashMap::new(),
            monitoring: Stats::default(),
            local_observed: false,
            error: None,
        }
    }

    fn apply(&mut self, reply: Reply) {
        let current = reply.job.generation == self.generation
            && self.target.as_ref() == Some(&reply.job.target);
        // A successfully priced token increment belongs to this monitoring run even after /resume.
        if let Ok(Reading::Tokens { increment, .. }) = &reply.result {
            self.monitoring.amount += increment;
            self.monitoring.requests = None;
            self.local_observed = true;
        }
        if !current {
            return;
        }
        self.pending = false;
        match reply.result {
            Ok(reading) => {
                let recovered = match reading {
                    Reading::Session(stats, recovered) => {
                        let previous = self
                            .accounted
                            .entry(reply.job.target)
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
                        recovered
                    }
                    Reading::Tokens { session, .. } => {
                        self.stats = Some(session);
                        false
                    }
                };
                self.reconnected |= self.interrupted || recovered;
                self.error = None;
                self.failures = 0;
                self.next = None;
            }
            Err(error) => {
                self.error = Some(error);
                self.interrupted = true;
                self.failures = self.failures.saturating_add(1);
                self.next = Some(
                    Instant::now()
                        + Duration::from_secs(1u64 << self.failures.min(6))
                            .min(Duration::from_secs(60)),
                );
            }
        }
    }

    /// Returns true when a billing/price query is submitted, so payment conversion can refresh.
    pub fn update(&mut self, observation: &Observation) -> bool {
        let target = observation.context();
        if self.target != target {
            self.target = target;
            self.generation += 1;
            self.pending = false;
            self.stats = None;
            self.error = None;
            self.failures = 0;
            self.next = None;
            self.revision = observation.revision;
            self.dirty = self.target.is_some();
        }
        if self.target.is_some()
            && (self.revision != observation.revision
                || self.model != observation.model
                || self.usage != observation.usage)
        {
            self.revision = observation.revision;
            self.dirty = true;
        }
        self.model = observation.model.clone();
        self.usage = observation.usage.clone();
        while let Ok(reply) = self.replies.try_recv() {
            self.apply(reply);
        }
        if !self.pending
            && (self.dirty || self.next.is_some_and(|next| Instant::now() >= next))
            && let Some(target) = &self.target
        {
            if self
                .jobs
                .send(Job {
                    generation: self.generation,
                    target: target.clone(),
                    model: self.model.clone(),
                    usage: self.usage.clone(),
                })
                .is_ok()
            {
                self.pending = true;
                self.dirty = false;
                self.next = None;
                return true;
            }
            self.error = Some("cost worker stopped".into());
            self.interrupted = true;
            self.next = Some(Instant::now() + Duration::from_secs(60));
        }
        false
    }

    fn status(&self) -> CostStatus {
        if self.error.is_some() {
            CostStatus::Reconnecting
        } else if self.stats.is_none() {
            CostStatus::Connecting
        } else if self.reconnected {
            CostStatus::Reconnected
        } else {
            CostStatus::Connected
        }
    }

    fn display_stats(&self, stats: Option<&Stats>, payment: Option<&PaymentInfo>) -> CostDisplay {
        let Some(stats) = stats else {
            return CostDisplay {
                status: self.status(),
                ..CostDisplay::default()
            };
        };
        let currency = &self.config.billing_currency;
        let prefix = if currency == "USD" { "$" } else { "" };
        let converted = payment
            .map(|payment| {
                format!(
                    " ({:.6} {})",
                    stats.amount * payment.exchange_rate,
                    payment.payment_currency
                )
            })
            .unwrap_or_default();
        CostDisplay {
            amount: Some(format!("{prefix}{:.8} {currency}{converted}", stats.amount)),
            requests: stats.requests,
            estimated: self.local,
            status: self.status(),
        }
    }

    pub fn session_display(&self, payment: Option<&PaymentInfo>) -> CostDisplay {
        self.display_stats(self.stats.as_ref(), payment)
    }

    pub fn monitoring_display(&self, payment: Option<&PaymentInfo>) -> CostDisplay {
        let stats = (!self.accounted.is_empty() || self.local_observed).then_some(&self.monitoring);
        self.display_stats(stats, payment)
    }

    pub fn session_line(&self, payment: Option<&PaymentInfo>) -> String {
        self.session_display(payment).to_string()
    }

    pub fn monitoring_line(&self, payment: Option<&PaymentInfo>) -> String {
        self.monitoring_display(payment).to_string()
    }

    pub fn detail(&self) -> &str {
        self.error.as_deref().unwrap_or("")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::dest::{SessionPricing, TokenPrices, TokenPricing};

    struct FakeSessionDestination;
    impl Destination for FakeSessionDestination {
        fn config(&self) -> DestinationConfig {
            DestinationConfig {
                name: "Fake".into(),
                billing_currency: "USD".into(),
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

    struct FakeTokenDestination;
    impl Destination for FakeTokenDestination {
        fn config(&self) -> DestinationConfig {
            DestinationConfig {
                name: "Token fake".into(),
                billing_currency: "CNY".into(),
            }
        }
        fn pricing(&self) -> PricingInterface<'_> {
            PricingInterface::TokenPrices(self)
        }
    }
    impl TokenPricing for FakeTokenDestination {
        fn token_prices(&self, _: &SessionContext, model: &str) -> Result<TokenPrices> {
            if !matches!(model, "test-model" | "cheap-model") {
                return Err("unknown model".into());
            }
            let multiplier = if model == "cheap-model" { 0.5 } else { 1.0 };
            Ok(TokenPrices {
                input: 2.0 * multiplier,
                cached_input: 0.5 * multiplier,
                cache_write_input: 2.0 * multiplier,
                output: 10.0 * multiplier,
                reasoning_output: 10.0 * multiplier,
            })
        }
    }

    #[test]
    fn token_destination_estimates_both_totals_and_preserves_monitoring_across_switches() {
        let mut monitor = Monitor::new(Arc::new(FakeTokenDestination));
        let mut observation = Observation {
            session_id: Some("a".into()),
            model: Some("test-model".into()),
            usage: Some(TokenUsage {
                input_tokens: 1_000_000,
                cached_input_tokens: 200_000,
                output_tokens: 100_000,
                ..Default::default()
            }),
            ..Default::default()
        };
        let observe = |monitor: &mut Monitor, observation: &Observation| {
            assert!(monitor.update(observation));
            let reply = monitor
                .replies
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            monitor.apply(reply);
        };
        observe(&mut monitor, &observation);
        assert_eq!(
            monitor.session_line(None),
            "2.70000000 CNY · Estimated · Connected"
        );
        assert_eq!(
            monitor.monitoring_line(None),
            "0.00000000 CNY · Estimated · Connected"
        );
        observation.usage.as_mut().unwrap().input_tokens += 500_000;
        observation.revision += 1;
        observe(&mut monitor, &observation);
        assert!((monitor.monitoring.amount - 1.0).abs() < 1e-10);
        // A completion event with unchanged totals must not charge again.
        observation.revision += 1;
        observe(&mut monitor, &observation);
        assert!((monitor.monitoring.amount - 1.0).abs() < 1e-10);
        observation.session_id = Some("b".into());
        observation.usage = Some(TokenUsage {
            input_tokens: 500_000,
            ..Default::default()
        });
        observe(&mut monitor, &observation);
        assert!((monitor.monitoring.amount - 1.0).abs() < 1e-10);
        observation.session_id = Some("a".into());
        observation.usage = Some(TokenUsage {
            input_tokens: 2_000_000,
            cached_input_tokens: 200_000,
            output_tokens: 100_000,
            ..Default::default()
        });
        observe(&mut monitor, &observation);
        assert_eq!(
            monitor.session_line(None),
            "4.70000000 CNY · Estimated · Connected"
        );
        assert_eq!(
            monitor.monitoring_line(None),
            "2.00000000 CNY · Estimated · Connected"
        );
        assert!(!monitor.monitoring_line(None).contains("Requests"));
        // A price lookup failure must not advance the worker's baseline.
        observation.model = Some("unknown".into());
        observation.usage.as_mut().unwrap().input_tokens += 500_000;
        observe(&mut monitor, &observation);
        assert!(monitor.error.as_ref().unwrap().contains("unknown model"));
        observation.model = Some("test-model".into());
        observe(&mut monitor, &observation);
        assert_eq!(
            monitor.monitoring_line(None),
            "3.00000000 CNY · Estimated · Reconnected"
        );
        observation.model = Some("cheap-model".into());
        observation.usage.as_mut().unwrap().input_tokens += 500_000;
        observe(&mut monitor, &observation);
        assert_eq!(
            monitor.session_line(None),
            "6.20000000 CNY · Estimated · Reconnected"
        );
        assert_eq!(
            monitor.monitoring_line(None),
            "3.50000000 CNY · Estimated · Reconnected"
        );
    }

    #[test]
    fn late_token_reply_adds_run_cost_without_overwriting_current_session() {
        let mut monitor = Monitor::new(Arc::new(FakeTokenDestination));
        monitor.target = Some(SessionContext {
            session_id: "new".into(),
            credential_profile: None,
        });
        monitor.generation = 2;
        monitor.pending = true;
        monitor.apply(Reply {
            job: Job {
                generation: 1,
                target: SessionContext {
                    session_id: "old".into(),
                    credential_profile: None,
                },
                model: None,
                usage: None,
            },
            result: Ok(Reading::Tokens {
                session: Stats {
                    amount: 20.0,
                    requests: None,
                },
                increment: 0.5,
            }),
        });
        assert_eq!(monitor.monitoring.amount, 0.5);
        assert!(monitor.stats.is_none());
        assert!(monitor.pending);
    }

    #[test]
    fn monitoring_counts_only_increases_after_each_baseline_across_session_switches() {
        let mut monitor = test_monitor();
        let (sender, receiver) = mpsc::channel();
        monitor.jobs = sender;
        let mut observe = |session_id: &str, usd, requests| {
            let session = Observation {
                session_id: Some(session_id.into()),
                revision: monitor.revision + 1,
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
        assert_eq!(
            monitor.monitoring,
            Stats {
                amount: 3.5,
                requests: Some(6)
            }
        );
        assert_eq!(
            monitor.stats,
            Some(Stats {
                amount: 13.0,
                requests: Some(105)
            })
        );
        assert_eq!(
            monitor.session_line(Some(&PaymentInfo {
                payment_currency: "CNY".into(),
                exchange_rate: 0.14
            })),
            "$13.00000000 USD (1.820000 CNY) | 105 Requests · Connected"
        );
        assert_eq!(
            monitor.monitoring_line(Some(&PaymentInfo {
                payment_currency: "CNY".into(),
                exchange_rate: 0.14
            })),
            "$3.50000000 USD (0.490000 CNY) | 6 Requests · Connected"
        );
        let job = Job {
            generation: monitor.generation,
            model: None,
            usage: None,
            target: monitor.target.clone().unwrap(),
        };
        monitor.apply(Reply {
            job,
            result: Err("network error".into()),
        });
        assert!(monitor.monitoring_line(None).ends_with("Reconnecting..."));
        assert_eq!(monitor.monitoring.requests, Some(6));
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
        assert_eq!(m.session_line(None), "Connecting...");
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
        assert_eq!(
            m.session_line(None),
            "$1.00000000 USD | 1 Requests · Connected"
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
            usage: None,
            target: SessionContext {
                session_id: "chat".into(),
                credential_profile: None,
            },
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
        assert!(m.session_line(None).ends_with("Connected"));
        m.apply(Reply {
            job: job(),
            result: Err("network error".into()),
        });
        assert_eq!(
            m.session_line(None),
            "$1.00000000 USD | 1 Requests · Reconnecting..."
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
        assert!(m.session_line(None).ends_with("Reconnected"));
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
        assert!(m.session_line(None).ends_with("Reconnected"));
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
            m.session_line(None).ends_with("Reconnected"),
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
                usage: None,
                target: SessionContext {
                    session_id: "old".into(),
                    credential_profile: None,
                },
            },
            result: Ok(Reading::Session(
                Stats {
                    amount: 99.0,
                    requests: Some(1),
                },
                false,
            )),
        });
        assert!(monitor.stats.is_none());
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
                usage: None,
                target: monitor.target.clone().unwrap(),
            },
            result: Ok(Reading::Session(
                Stats {
                    amount: 99.0,
                    requests: Some(1),
                },
                false,
            )),
        });
        assert!(monitor.stats.is_none());
    }
}
