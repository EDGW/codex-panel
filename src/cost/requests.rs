//! Pending response snapshots and per-response retry policy.
use super::{Job, retry_delay};
use crate::dest::{SessionContext, TokenRequest};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::time::Instant;

struct Failure {
    retry_at: Instant,
    attempts: u32,
    error: String,
}

#[derive(Default)]
pub(super) struct RequestQueue {
    received: HashSet<u64>,
    jobs: VecDeque<Job>,
    failures: BTreeMap<u64, Failure>,
}

impl RequestQueue {
    pub fn observe(&mut self, requests: &[TokenRequest], generation: u64) {
        for request in requests {
            if self.received.insert(request.sequence) {
                self.jobs.push_back(Job {
                    generation,
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

    pub fn ready(&self, now: Instant) -> Option<&Job> {
        self.jobs.iter().find(|job| {
            let request = job.request.as_ref().expect("queued response snapshot");
            self.failures
                .get(&request.sequence)
                .is_none_or(|failure| now >= failure.retry_at)
        })
    }

    pub fn complete(&mut self, sequence: u64) {
        self.failures.remove(&sequence);
        self.jobs.retain(|job| {
            job.request
                .as_ref()
                .expect("queued response snapshot")
                .sequence
                != sequence
        });
    }

    pub fn fail(&mut self, request: &TokenRequest, error: &str) {
        let attempts = self
            .failures
            .get(&request.sequence)
            .map_or(1, |failure| failure.attempts.saturating_add(1));
        self.failures.insert(
            request.sequence,
            Failure {
                retry_at: Instant::now() + retry_delay(attempts),
                attempts,
                error: format!(
                    "Response {} (session {}, model {}): {error}",
                    request.sequence,
                    request.session_id,
                    request.model.as_deref().unwrap_or("unavailable")
                ),
            },
        );
    }

    pub fn error(&self) -> Option<&str> {
        self.failures
            .values()
            .next()
            .map(|failure| failure.error.as_str())
    }

    #[cfg(test)]
    pub fn retry_now(&mut self, sequence: u64) {
        self.failures
            .get_mut(&sequence)
            .expect("failed response")
            .retry_at = Instant::now();
    }
}
