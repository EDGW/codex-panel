use crate::dest::{
    Destination, DestinationConfig, PaymentInfo, PricingInterface, Result, SessionContext, Stats,
    TokenQuote, TokenRequest, TokenUsage,
};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
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
    NotAvailable,
}

impl CostStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Connecting => "Connecting...",
            Self::Connected => "Connected",
            Self::Reconnected => "Reconnected",
            Self::Reconnecting => "Reconnecting...",
            Self::Unavailable => "Unavailable",
            Self::NotAvailable => "Not available",
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

/// Price a single immutable response snapshot; never infer request usage from session totals.
fn query_tokens(destination: &dyn Destination, job: &Job) -> Result<Reading> {
    let PricingInterface::TokenPrices(pricing) = destination.pricing() else {
        return Err("destination has no token pricing interface".into());
    };
    let model = job
        .model
        .as_deref()
        .ok_or("model unavailable for request pricing")?;
    let quote = pricing.token_prices(job.target.as_ref(), model)?;
    if quote.currency.trim().is_empty() {
        return Err("token quote has no currency".into());
    }
    quote.prices.estimate(&TokenUsage::default())?;
    match &job.request {
        Some(request) => Ok(Reading::Tokens {
            currency: quote.currency,
            amount: quote.prices.estimate(&request.usage)?,
        }),
        None => Ok(Reading::Quote(quote)),
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
    received_requests: HashSet<u64>,
    request_queue: VecDeque<Job>,
    dirty: bool,
    interrupted: bool,
    reconnected: bool,
    failures: u32,
    stats: Option<Stats>,
    accounted: HashMap<SessionContext, Stats>,
    monitoring: Stats,
    token_monitoring: BTreeMap<String, Stats>,
    error: Option<String>,
}

impl Monitor {
    pub fn new(destination: Arc<dyn Destination>) -> Self {
        let config = destination.config();
        let local = matches!(destination.pricing(), PricingInterface::TokenPrices(_));
        let (jobs, receiver) = mpsc::channel::<Job>();
        let (sender, replies) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(mut job) = receiver.recv() {
                // Only cumulative session reads can safely skip obsolete queued jobs.
                if !local {
                    while let Ok(latest) = receiver.try_recv() {
                        job = latest;
                    }
                }
                let result = match destination.pricing() {
                    PricingInterface::SessionTotals(pricing) => destination
                        .config()
                        .billing_currency
                        .filter(|currency| !currency.trim().is_empty())
                        .ok_or_else(|| "session pricing has no billing currency".to_owned())
                        .and_then(|_| {
                            pricing
                                .session_totals(job.target.as_ref().ok_or("session unavailable")?)
                        })
                        .and_then(|(stats, recovered)| {
                            stats.validate()?;
                            Ok(Reading::Session(stats, recovered))
                        }),
                    PricingInterface::TokenPrices(_) => query_tokens(destination.as_ref(), &job),
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
            received_requests: HashSet::new(),
            request_queue: VecDeque::new(),
            dirty: false,
            interrupted: false,
            reconnected: false,
            failures: 0,
            stats: None,
            accounted: HashMap::new(),
            monitoring: Stats::default(),
            token_monitoring: BTreeMap::new(),
            error: None,
        }
    }

    fn apply(&mut self, reply: Reply) {
        let current = reply.job.generation == self.generation && self.target == reply.job.target;
        if self.local {
            self.pending = false;
            if reply.job.request.is_none() && (reply.job.model != self.model || !current) {
                return;
            }
            if reply.result.is_ok() && reply.job.request.is_some() {
                self.request_queue.pop_front();
            }
        } else {
            if !current {
                return;
            }
            self.pending = false;
        }
        match reply.result {
            Ok(reading) => {
                let recovered = match reading {
                    Reading::Session(stats, recovered) => {
                        let previous = self
                            .accounted
                            .entry(reply.job.target.expect("session totals have a target"))
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
                    Reading::Quote(quote) if reply.job.model == self.model => {
                        self.token_monitoring
                            .entry(quote.currency)
                            .or_insert(Stats {
                                amount: 0.0,
                                requests: Some(0),
                            });
                        false
                    }
                    Reading::Quote(_) => false,
                    Reading::Tokens { currency, amount } => {
                        let sum = self.token_monitoring.entry(currency).or_insert(Stats {
                            amount: 0.0,
                            requests: Some(0),
                        });
                        sum.amount += amount;
                        sum.requests = Some(sum.requests.unwrap_or(0).saturating_add(1));
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
        if self.local {
            for request in &observation.requests {
                if self.received_requests.insert(request.sequence) {
                    self.request_queue.push_back(Job {
                        generation: self.generation,
                        target: Some(SessionContext {
                            session_id: request.session_id.clone(),
                            credential_profile: request.credential_profile.clone(),
                        }),
                        model: request.model.clone(),
                        request: Some(request.clone()),
                    });
                }
            }
        }
        if self.target != target {
            self.target = target;
            self.generation += 1;
            if !self.local {
                self.pending = false;
            }
            self.stats = None;
            if !self.local {
                self.error = None;
                self.failures = 0;
                self.next = None;
            }
            self.revision = observation.revision;
            self.dirty = self.target.is_some();
        }
        if (self.local || self.target.is_some())
            && (self.model != observation.model
                || !self.local
                    && (self.revision != observation.revision || self.usage != observation.usage))
        {
            self.revision = observation.revision;
            self.dirty = true;
        }
        self.model = observation.model.clone();
        self.usage = observation.usage.clone();
        while let Ok(reply) = self.replies.try_recv() {
            self.apply(reply);
        }
        let retry_ready = self.next.is_none_or(|next| Instant::now() >= next);
        if !self.pending
            && retry_ready
            && (!self.local || self.model.is_some() || !self.request_queue.is_empty())
            && (self.dirty || !self.request_queue.is_empty() || self.next.is_some())
            && (self.local || self.target.is_some())
        {
            let job = self
                .request_queue
                .front()
                .map(|job| Job {
                    generation: job.generation,
                    target: job.target.clone(),
                    model: job.model.clone(),
                    request: job.request.clone(),
                })
                .unwrap_or(Job {
                    generation: self.generation,
                    target: self.target.clone(),
                    model: self.model.clone(),
                    request: None,
                });
            let prefetch = job.request.is_none();
            if self.jobs.send(job).is_ok() {
                self.pending = true;
                if prefetch {
                    self.dirty = false;
                }
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
        } else if self.local && self.model.is_none() {
            CostStatus::Connected
        } else if !self.local && self.stats.is_none() {
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
        let Some(currency) = &self.config.billing_currency else {
            return CostDisplay {
                status: CostStatus::Unavailable,
                ..CostDisplay::default()
            };
        };
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
        if self.local {
            return CostDisplay {
                status: CostStatus::NotAvailable,
                estimated: true,
                ..Default::default()
            };
        }
        self.display_stats(self.stats.as_ref(), payment)
    }

    pub fn monitoring_display(&self, payment: Option<&PaymentInfo>) -> CostDisplay {
        if self.local {
            return self.display_tokens(&self.token_monitoring, payment);
        }
        let stats = (!self.accounted.is_empty()).then_some(&self.monitoring);
        self.display_stats(stats, payment)
    }

    fn display_tokens(
        &self,
        amounts: &BTreeMap<String, Stats>,
        payment: Option<&PaymentInfo>,
    ) -> CostDisplay {
        let amount = (!amounts.is_empty()).then(|| {
            amounts
                .iter()
                .map(|(currency, stats)| {
                    let prefix = if currency == "USD" { "$" } else { "" };
                    let converted = payment
                        .filter(|_| self.config.billing_currency.as_ref() == Some(currency))
                        .map(|payment| {
                            format!(
                                " ({:.6} {})",
                                stats.amount * payment.exchange_rate,
                                payment.payment_currency
                            )
                        })
                        .unwrap_or_default();
                    format!("{prefix}{:.8} {currency}{converted}", stats.amount)
                })
                .collect::<Vec<_>>()
                .join(" + ")
        });
        let amount = amount.or_else(|| {
            let currency = self.config.billing_currency.as_deref().unwrap_or("");
            let prefix = if currency == "USD" { "$" } else { "" };
            Some(if currency.is_empty() {
                "0.00000000".into()
            } else {
                format!("{prefix}0.00000000 {currency}")
            })
        });
        CostDisplay {
            amount,
            requests: Some(amounts.values().fold(0u64, |total, stats| {
                total.saturating_add(stats.requests.unwrap_or(0))
            })),
            estimated: true,
            status: self.status(),
        }
    }

    pub fn billing_currencies(&self) -> String {
        if self.local {
            self.token_monitoring
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(" + ")
        } else {
            self.config.billing_currency.clone().unwrap_or_default()
        }
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
    use crate::dest::{SessionPricing, TokenPrices, TokenPricing, TokenQuote};

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
        assert_eq!(monitor.session_line(None), "Not available");
        assert_eq!(
            monitor.monitoring_line(None),
            "0.00000000 CNY | 0 Requests · Estimated · Connected"
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
        assert_eq!(
            monitor.monitoring_line(None),
            "3.00000000 CNY | 2 Requests · Estimated · Connected"
        );
        assert!(!monitor.update(&observation));
        destination
            .fail_next
            .store(true, std::sync::atomic::Ordering::Relaxed);
        observation.requests.push(request(3, "test-model"));
        settle(&mut monitor, &observation);
        assert_eq!(monitor.token_monitoring["CNY"].amount, 3.0);
        assert_eq!(monitor.token_monitoring["CNY"].requests, Some(2));
        observation.model = Some("cheap-model".into());
        observation.session_id = Some("other-session".into());
        assert!(!monitor.update(&observation));
        assert_eq!(
            monitor.request_queue.front().unwrap().model.as_deref(),
            Some("test-model")
        );
        // Retry the original response after a temporary source failure.
        monitor.next = Some(Instant::now());
        settle(&mut monitor, &observation);
        assert_eq!(
            monitor.monitoring_line(None),
            "5.00000000 CNY | 3 Requests · Estimated · Reconnected"
        );
        assert_eq!(monitor.session_line(None), "Not available");
        // The current model still receives its price prefetch after older responses settle.
        settle(&mut monitor, &observation);
        assert_eq!(monitor.monitoring_display(None).requests, Some(3));
        observation.requests.push(request(4, "other-currency"));
        settle(&mut monitor, &observation);
        assert_eq!(
            monitor.monitoring_line(None),
            "5.00000000 CNY + $2.00000000 USD | 4 Requests · Estimated · Reconnected"
        );
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
            request: None,
            target: monitor.target.clone(),
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
        assert!(monitor.stats.is_none());
    }
}
