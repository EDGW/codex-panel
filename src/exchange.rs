//! Conversion scheduling: expiry-gated events, retry backoff and retained last rates.
use crate::conversion::{ConversionSettings, PaymentConversion};
use crate::dest::{PaymentInfo, Result};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

struct Worker {
    jobs: mpsc::Sender<()>,
    replies: mpsc::Receiver<Result<PaymentInfo>>,
    cache: Duration,
    pending: bool,
    dirty: bool,
    expires: Option<Instant>,
    retry: Option<Instant>,
    failures: u32,
}

pub struct Exchange {
    worker: Option<Worker>,
    pub payment: Option<PaymentInfo>,
    error: Option<String>,
    configured: bool,
    settings: Option<ConversionSettings>,
}

#[derive(Clone, Debug, Default)]
pub struct ConversionDisplay {
    pub configured: bool,
    pub settings: Option<ConversionSettings>,
    pub payment: Option<PaymentInfo>,
    pub status: String,
    pub warning: bool,
}

impl Exchange {
    pub fn new(conversion: Option<Arc<dyn PaymentConversion>>) -> Result<Self> {
        let Some(conversion) = conversion else {
            return Ok(Self {
                worker: None,
                payment: None,
                error: None,
                configured: false,
                settings: None,
            });
        };
        let settings = conversion.settings();
        if let Some(payment) = conversion.initial_payment()? {
            payment.validate()?;
            return Ok(Self {
                worker: None,
                payment: Some(payment),
                error: None,
                configured: true,
                settings,
            });
        }
        let cache = conversion.cache_duration();
        let (jobs, receiver) = mpsc::channel();
        let (sender, replies) = mpsc::channel();
        thread::spawn(move || {
            while receiver.recv().is_ok() {
                let result = conversion.payment_info().and_then(|payment| {
                    payment.validate()?;
                    Ok(payment)
                });
                if sender.send(result).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            worker: Some(Worker {
                jobs,
                replies,
                cache,
                pending: false,
                dirty: true,
                expires: None,
                retry: None,
                failures: 0,
            }),
            payment: None,
            error: None,
            configured: true,
            settings,
        })
    }

    pub fn display(&self) -> ConversionDisplay {
        ConversionDisplay {
            configured: self.configured,
            settings: self.settings.clone(),
            payment: self.payment.clone(),
            warning: self.error.is_some(),
            status: if !self.configured {
                "Not configured or disabled".into()
            } else if self.error.is_some() {
                self.detail()
            } else if self.payment.is_none() {
                "Connecting...".into()
            } else if self.worker.as_ref().is_some_and(|worker| worker.pending) {
                "Refreshing (using last known rate)".into()
            } else {
                "Ready".into()
            },
        }
    }

    pub fn update(&mut self, refresh: bool) {
        let Some(worker) = &mut self.worker else {
            return;
        };
        worker.dirty |= refresh;
        while let Ok(result) = worker.replies.try_recv() {
            worker.pending = false;
            match result {
                Ok(payment) => {
                    self.payment = Some(payment);
                    self.error = None;
                    worker.failures = 0;
                    worker.retry = None;
                    worker.expires = Some(Instant::now() + worker.cache);
                }
                Err(error) => {
                    self.error = Some(error);
                    worker.failures = worker.failures.saturating_add(1);
                    worker.retry = Some(
                        Instant::now()
                            + Duration::from_secs(1u64 << worker.failures.min(6))
                                .min(Duration::from_secs(60)),
                    );
                }
            }
        }
        let now = Instant::now();
        let expired = worker.expires.is_none_or(|expiry| now >= expiry);
        if !expired && worker.retry.is_none() {
            worker.dirty = false;
        }
        let should_query = match worker.retry {
            Some(retry) => now >= retry,
            None => worker.dirty && expired,
        };
        if !worker.pending && should_query {
            if worker.jobs.send(()).is_ok() {
                worker.pending = true;
                worker.dirty = false;
                worker.retry = None;
            } else {
                self.error = Some("payment conversion worker stopped".into());
                worker.retry = Some(now + Duration::from_secs(60));
            }
        }
    }

    pub fn detail(&self) -> String {
        if let Some(error) = &self.error {
            format!(
                "{}: {error}",
                if self.payment.is_some() {
                    "Payment conversion uses last known rate (stale)"
                } else {
                    "Payment conversion unavailable"
                }
            )
        } else if self.worker.is_some() && self.payment.is_none() {
            "Payment conversion: Connecting...".into()
        } else {
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversion::config;

    fn configured(text: &str) -> Arc<dyn PaymentConversion> {
        config::parse(&text.parse().unwrap(), true)
            .unwrap()
            .unwrap()
    }
    fn payment(rate: f64) -> PaymentInfo {
        PaymentInfo {
            payment_currency: "CNY".into(),
            exchange_rate: rate,
        }
    }
    #[test]
    fn absent_and_fixed_conversion_are_ready_without_a_worker() {
        let mut absent = Exchange::new(None).unwrap();
        absent.update(true);
        assert!(absent.worker.is_none());
        assert_eq!(absent.detail(), "");
        let fixed = Exchange::new(Some(configured(
            "currency='CNY'\nmultiplier=2.0\n[source]\ntype='value'\nvalue=0.14",
        )))
        .unwrap();
        assert!(fixed.worker.is_none());
        assert_eq!(fixed.payment, Some(payment(0.28)));
        assert_eq!(fixed.detail(), "");
    }
    #[test]
    fn cache_gates_events_and_errors_retry_without_losing_previous_rate() {
        let mut exchange = Exchange::new(None).unwrap();
        let (jobs, receiver) = mpsc::channel();
        let (sender, replies) = mpsc::channel();
        exchange.worker = Some(Worker {
            jobs,
            replies,
            cache: Duration::from_secs(300),
            pending: false,
            dirty: true,
            expires: None,
            retry: None,
            failures: 0,
        });
        exchange.update(false);
        receiver.try_recv().unwrap();
        exchange.update(true);
        sender.send(Ok(payment(0.14))).unwrap();
        exchange.update(false);
        assert!(
            receiver.try_recv().is_err(),
            "fresh cache fulfills in-flight events"
        );
        exchange.update(true);
        assert!(receiver.try_recv().is_err());
        exchange.worker.as_mut().unwrap().expires = Some(Instant::now());
        exchange.update(false);
        assert!(receiver.try_recv().is_err(), "expiry alone does not poll");
        exchange.update(true);
        receiver.try_recv().unwrap();
        sender.send(Err("network error".into())).unwrap();
        exchange.update(false);
        assert_eq!(exchange.payment, Some(payment(0.14)));
        assert!(exchange.detail().contains("stale"));
        exchange.update(true);
        assert!(
            receiver.try_recv().is_err(),
            "billing events respect backoff"
        );
        exchange.worker.as_mut().unwrap().retry = Some(Instant::now());
        exchange.update(false);
        receiver.try_recv().unwrap();
        sender.send(Ok(payment(0.15))).unwrap();
        exchange.update(false);
        assert_eq!(exchange.payment, Some(payment(0.15)));
        assert_eq!(exchange.detail(), "");
    }
}
