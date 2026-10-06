//! Background protocol queries; no accounting or presentation policy.
use super::{Job, Reading, Reply};
use crate::dest::{Destination, PricingInterface, Result, TokenUsage};
use std::sync::{Arc, mpsc};
use std::thread;

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

pub(super) fn spawn(
    destination: Arc<dyn Destination>,
) -> (mpsc::Sender<Job>, mpsc::Receiver<Reply>) {
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
                        pricing.session_totals(job.target.as_ref().ok_or("session unavailable")?)
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
    (jobs, replies)
}
