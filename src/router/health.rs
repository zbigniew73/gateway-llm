use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::RoutingConfig;

#[derive(Debug, Default)]
struct DeploymentHealth {
    failures: Vec<Instant>,
    cooldown_until: Option<Instant>,
}

#[derive(Debug)]
pub struct HealthTracker {
    inner: Mutex<HashMap<String, DeploymentHealth>>,
    error_threshold: u32,
    error_window: Duration,
    cooldown: Duration,
}

impl HealthTracker {
    pub fn new(routing: &RoutingConfig) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            error_threshold: routing.error_threshold.max(1),
            error_window: Duration::from_secs(routing.error_window_seconds.max(1)),
            cooldown: Duration::from_secs(routing.cooldown_seconds.max(1)),
        }
    }

    pub fn is_available(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        match guard.get_mut(key) {
            Some(entry) => match entry.cooldown_until {
                Some(until) if until > now => false,
                Some(_) => {
                    entry.cooldown_until = None;
                    entry.failures.clear();
                    true
                }
                None => true,
            },
            None => true,
        }
    }

    pub fn record_success(&self, key: &str) {
        let mut guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(entry) = guard.get_mut(key) {
            entry.failures.clear();
            entry.cooldown_until = None;
        }
    }

    pub fn record_failure(&self, key: &str) -> bool {
        let now = Instant::now();
        let window = self.error_window;
        let threshold = self.error_threshold as usize;

        let mut guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let entry = guard.entry(key.to_string()).or_default();
        entry
            .failures
            .retain(|moment| now.duration_since(*moment) <= window);
        entry.failures.push(now);

        if entry.failures.len() >= threshold {
            entry.failures.clear();
            entry.cooldown_until = Some(now + self.cooldown);
            true
        } else {
            false
        }
    }

    pub fn cooldown_remaining_secs(&self, key: &str) -> u64 {
        let now = Instant::now();
        let guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard
            .get(key)
            .and_then(|entry| entry.cooldown_until)
            .filter(|until| *until > now)
            .map(|until| until.duration_since(now).as_secs() + 1)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn routing(threshold: u32) -> RoutingConfig {
        RoutingConfig {
            connect_timeout_seconds: 20,
            stream_idle_timeout_seconds: 90,
            non_stream_timeout_seconds: 300,
            error_threshold: threshold,
            error_window_seconds: 120,
            cooldown_seconds: 60,
        }
    }

    #[test]
    fn cooldown_starts_after_threshold_failures() {
        let tracker = HealthTracker::new(&routing(3));
        assert!(tracker.is_available("a"));

        assert!(!tracker.record_failure("a"));
        assert!(tracker.is_available("a"));
        assert!(!tracker.record_failure("a"));
        assert!(tracker.is_available("a"));
        assert!(tracker.record_failure("a"));

        assert!(!tracker.is_available("a"));
        assert!(tracker.cooldown_remaining_secs("a") > 0);
    }

    #[test]
    fn success_resets_failures() {
        let tracker = HealthTracker::new(&routing(2));
        assert!(!tracker.record_failure("b"));
        tracker.record_success("b");
        assert!(!tracker.record_failure("b"));
        assert!(tracker.is_available("b"));
    }
}
