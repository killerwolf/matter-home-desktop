//! Limitation des tentatives de connexion : après quelques échecs, l'adresse (puis, plus
//! largement, le serveur entier) est mise en attente, avec un délai qui double.
//! État en mémoire : un redémarrage le remet à zéro (un attaquant distant ne peut pas
//! redémarrer le serveur).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Échecs tolérés par adresse, puis au total, avant la première attente.
const IP_FREE_FAILURES: u32 = 5;
const GLOBAL_FREE_FAILURES: u32 = 20;
/// Première attente, doublée à chaque échec de plus, plafonnée.
const BASE_DELAY: Duration = Duration::from_secs(30);
const MAX_DELAY: Duration = Duration::from_secs(15 * 60);
/// Sans nouvel échec pendant ce temps, le compteur repart à zéro.
const FORGET_AFTER: Duration = Duration::from_secs(3600);
/// Nombre maximal d'adresses suivies (mémoire bornée).
const MAX_TRACKED_IPS: usize = 1024;

#[derive(Default, Clone, Copy)]
struct Entry {
    failures: u32,
    last_failure: Option<Instant>,
    locked_until: Option<Instant>,
}

impl Entry {
    fn expire(&mut self, now: Instant) {
        if self
            .last_failure
            .is_some_and(|t| now.duration_since(t) > FORGET_AFTER)
        {
            *self = Entry::default();
        }
    }

    fn retry_after(&self, now: Instant) -> Option<Duration> {
        self.locked_until
            .and_then(|t| t.checked_duration_since(now))
            .filter(|d| !d.is_zero())
    }

    fn fail(&mut self, now: Instant, free: u32) {
        self.failures += 1;
        self.last_failure = Some(now);
        if self.failures >= free {
            let doublings = (self.failures - free).min(10);
            let delay = BASE_DELAY.saturating_mul(1 << doublings).min(MAX_DELAY);
            self.locked_until = Some(now + delay);
        }
    }
}

#[derive(Default)]
pub struct LoginLimiter {
    per_ip: Mutex<HashMap<IpAddr, Entry>>,
    global: Mutex<Entry>,
}

impl LoginLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// `Err(attente)` si cette adresse (ou le serveur) est en attente.
    pub fn check(&self, ip: IpAddr) -> Result<(), Duration> {
        self.check_at(ip, Instant::now())
    }

    pub fn record_failure(&self, ip: IpAddr) -> Option<Duration> {
        self.record_failure_at(ip, Instant::now())
    }

    pub fn record_success(&self, ip: IpAddr) {
        self.per_ip
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ip);
        *self.global.lock().unwrap_or_else(|e| e.into_inner()) = Entry::default();
    }

    fn check_at(&self, ip: IpAddr, now: Instant) -> Result<(), Duration> {
        let mut global = self.global.lock().unwrap_or_else(|e| e.into_inner());
        global.expire(now);
        let mut per_ip = self.per_ip.lock().unwrap_or_else(|e| e.into_inner());
        let ip_wait = per_ip.get_mut(&ip).and_then(|e| {
            e.expire(now);
            e.retry_after(now)
        });
        match ip_wait.max(global.retry_after(now)) {
            Some(wait) => Err(wait),
            None => Ok(()),
        }
    }

    /// Note un échec ; retourne l'attente qui en résulte, s'il y en a une.
    fn record_failure_at(&self, ip: IpAddr, now: Instant) -> Option<Duration> {
        let mut global = self.global.lock().unwrap_or_else(|e| e.into_inner());
        global.expire(now);
        global.fail(now, GLOBAL_FREE_FAILURES);
        let mut per_ip = self.per_ip.lock().unwrap_or_else(|e| e.into_inner());
        if per_ip.len() >= MAX_TRACKED_IPS && !per_ip.contains_key(&ip) {
            // Mémoire bornée : on oublie les adresses sans attente en cours.
            per_ip.retain(|_, e| e.retry_after(now).is_some());
            if per_ip.len() >= MAX_TRACKED_IPS {
                return global.retry_after(now);
            }
        }
        let entry = per_ip.entry(ip).or_default();
        entry.expire(now);
        entry.fail(now, IP_FREE_FAILURES);
        entry.retry_after(now).max(global.retry_after(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(n: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, n])
    }

    #[test]
    fn locks_after_free_failures_with_growing_delay() {
        let l = LoginLimiter::new();
        let t0 = Instant::now();
        for _ in 0..IP_FREE_FAILURES - 1 {
            assert!(l.record_failure_at(ip(1), t0).is_none());
            assert!(l.check_at(ip(1), t0).is_ok());
        }
        let wait = l.record_failure_at(ip(1), t0).unwrap();
        assert_eq!(wait, BASE_DELAY);
        assert_eq!(l.check_at(ip(1), t0).unwrap_err(), BASE_DELAY);
        // Une autre adresse n'est pas touchée.
        assert!(l.check_at(ip(2), t0).is_ok());
        // Après l'attente, un nouvel échec double le délai.
        let t1 = t0 + BASE_DELAY + Duration::from_secs(1);
        assert!(l.check_at(ip(1), t1).is_ok());
        assert_eq!(l.record_failure_at(ip(1), t1).unwrap(), BASE_DELAY * 2);
        // Plafond.
        let mut t = t1;
        for _ in 0..30 {
            t += MAX_DELAY + Duration::from_secs(1);
            l.record_failure_at(ip(1), t);
        }
        assert_eq!(l.check_at(ip(1), t).unwrap_err(), MAX_DELAY);
    }

    #[test]
    fn success_resets_and_old_failures_are_forgotten() {
        let l = LoginLimiter::new();
        let t0 = Instant::now();
        for _ in 0..IP_FREE_FAILURES {
            l.record_failure_at(ip(1), t0);
        }
        assert!(l.check_at(ip(1), t0).is_err());
        l.record_success(ip(1));
        assert!(l.check_at(ip(1), t0).is_ok());
        for _ in 0..IP_FREE_FAILURES - 1 {
            l.record_failure_at(ip(3), t0);
        }
        // Une heure plus tard le compteur est oublié : il faut de nouveau 5 échecs.
        let later = t0 + FORGET_AFTER + Duration::from_secs(1);
        assert!(l.record_failure_at(ip(3), later).is_none());
    }

    #[test]
    fn global_limit_covers_many_addresses() {
        let l = LoginLimiter::new();
        let t0 = Instant::now();
        for n in 0..GLOBAL_FREE_FAILURES as u8 {
            l.record_failure_at(ip(n), t0);
        }
        // Une adresse neuve est elle aussi mise en attente.
        assert!(l.check_at(ip(200), t0).is_err());
    }
}
