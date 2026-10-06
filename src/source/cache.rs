use crate::Result;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Only successful refreshes are cached; expired data never conceals source failures.
pub struct Cache<T> {
    duration: Duration,
    value: Mutex<Option<(Instant, T)>>,
}

impl<T: Clone> Cache<T> {
    pub fn new(duration: Duration) -> Self {
        Self {
            duration,
            value: Mutex::new(None),
        }
    }

    pub fn get(&self, retrieve: impl FnOnce() -> Result<T>) -> Result<T> {
        let mut cached = self
            .value
            .lock()
            .map_err(|_| "source cache lock poisoned")?;
        if let Some((at, value)) = cached.as_ref()
            && at.elapsed() < self.duration
        {
            return Ok(value.clone());
        }
        let value = retrieve()?;
        *cached = Some((Instant::now(), value.clone()));
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_failures_are_not_cached_or_hidden_by_expired_data() {
        let cache = Cache::new(Duration::from_secs(60));
        assert!(cache.get(|| Err::<u32, _>("offline".into())).is_err());
        assert_eq!(cache.get(|| Ok(7)).unwrap(), 7);
        assert_eq!(
            cache
                .get(|| panic!("unexpired cache should avoid retrieval"))
                .unwrap(),
            7
        );
        let expired = Cache::new(Duration::ZERO);
        assert_eq!(expired.get(|| Ok(3)).unwrap(), 3);
        assert!(expired.get(|| Err::<u32, _>("offline".into())).is_err());
        assert_eq!(expired.get(|| Ok(9)).unwrap(), 9);
    }
}
