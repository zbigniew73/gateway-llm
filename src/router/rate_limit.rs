//! Proaktywny token-bucket rate limiter per provider (nie per deployment/alias —
//! `rpm` w configu to limit na CAŁE konto danego providera, dzielony przez
//! wszystkie deploymenty, które go używają).
//!
//! W przeciwieństwie do `HealthTracker` (który reaguje POST FACTO na 429/5xx),
//! ten limiter ma nie dopuścić do wysłania żądania, które i tak dostałoby 429.
//! Providerzy bez skonfigurowanego `rpm` są zawsze dostępni (brak wpisu w
//! `buckets` == brak limitu).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use crate::config::ProviderConfig;

#[derive(Debug)]
struct Bucket {
    capacity: f64,
    tokens: f64,
    refill_per_sec: f64,
    last_refill: Instant,
}

impl Bucket {
    fn new(rpm: u32) -> Self {
        let capacity = f64::from(rpm.max(1));
        Self {
            capacity,
            tokens: capacity, // startujemy z pełnym kubełkiem, nie od zera
            refill_per_sec: capacity / 60.0,
            last_refill: Instant::now(),
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        if elapsed > 0.0 {
            self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
            self.last_refill = now;
        }
    }
}

#[derive(Debug)]
pub struct RateLimiter {
    buckets: HashMap<String, Mutex<Bucket>>,
}

impl RateLimiter {
    pub fn new(providers: &HashMap<String, ProviderConfig>) -> Self {
        let buckets = providers
            .iter()
            .filter_map(|(name, cfg)| cfg.rpm.map(|rpm| (name.clone(), Mutex::new(Bucket::new(rpm)))))
            .collect();
        Self { buckets }
    }

    /// Podgląd BEZ zużycia tokena — używany przy wyborze kolejności kandydatów
    /// (analogicznie do `HealthTracker::is_available`).
    pub fn has_capacity(&self, provider: &str) -> bool {
        let Some(lock) = self.buckets.get(provider) else {
            return true;
        };
        let mut bucket = match lock.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        bucket.refill(Instant::now());
        bucket.tokens >= 1.0
    }

    /// Faktycznie zużywa token, jeśli jest dostępny. Zwraca `false`, gdy
    /// kubełek jest pusty — wywołujący i tak wysyła żądanie (patrz
    /// `Router::dispatch_one`: to tylko księgowanie, nie bramka blokująca,
    /// żeby zachować tę samą filozofię co cooldown: "lepiej spróbować, niż
    /// nic"), ale wtedy świadomie ryzykuje 429 u providera.
    pub fn try_acquire(&self, provider: &str) -> bool {
        let Some(lock) = self.buckets.get(provider) else {
            return true;
        };
        let mut bucket = match lock.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        bucket.refill(Instant::now());
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    fn providers_with_rpm(name: &str, rpm: u32) -> HashMap<String, ProviderConfig> {
        let mut map = HashMap::new();
        map.insert(
            name.to_string(),
            ProviderConfig {
                base_url: "https://example.test".to_string(),
                chat_path: "/chat/completions".to_string(),
                rpm: Some(rpm),
                stream_usage: false,
            },
        );
        map
    }

    #[test]
    fn provider_without_configured_rpm_is_always_available() {
        let limiter = RateLimiter::new(&HashMap::new());
        assert!(limiter.has_capacity("anything"));
        for _ in 0..1000 {
            assert!(limiter.try_acquire("anything"));
        }
    }

    #[test]
    fn bucket_starts_full_and_then_denies() {
        let limiter = RateLimiter::new(&providers_with_rpm("p", 3));
        assert!(limiter.try_acquire("p"));
        assert!(limiter.try_acquire("p"));
        assert!(limiter.try_acquire("p"));
        assert!(!limiter.try_acquire("p"), "4. żądanie musi przekroczyć limit 3 RPM");
        assert!(!limiter.has_capacity("p"));
    }

    #[test]
    fn peeking_with_has_capacity_does_not_consume_tokens() {
        let limiter = RateLimiter::new(&providers_with_rpm("p", 2));
        for _ in 0..50 {
            assert!(limiter.has_capacity("p"));
        }
        assert!(limiter.try_acquire("p"));
        assert!(limiter.try_acquire("p"));
        assert!(!limiter.try_acquire("p"));
    }

    #[test]
    fn bucket_refills_over_time() {
        // 600 RPM => 10 tokenów/s, więc 150ms dolewa ~1.5 tokena.
        let limiter = RateLimiter::new(&providers_with_rpm("p", 600));
        for _ in 0..600 {
            assert!(limiter.try_acquire("p"));
        }
        assert!(!limiter.try_acquire("p"), "kubełek 600 RPM powinien być pusty po 600 zużyciach");

        sleep(Duration::from_millis(150));
        assert!(limiter.try_acquire("p"), "po ~150ms przy 600 RPM powinien odnowić się token");
    }
}
