//! Découverte mDNS : appareils en mode appairage (`_matterc._udp`) et nœuds
//! opérationnels (`_matter._tcp`). Une seule tâche consomme les événements mDNS
//! de matc et tient un cache que les autres tâches consultent.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use matc::discover::{extract_matter_info, CommissioningMode, MatterDeviceInfo};
use matc::mdns2::{MdnsEvent, MdnsService};
use tokio::sync::{Mutex, Notify};

use super::setup_code::SetupInfo;

const OPERATIONAL: &str = "_matter._tcp.local";
const COMMISSIONABLE: &str = "_matterc._udp.local";
const QTYPE_ANY: u16 = 0xff;
const QTYPE_AAAA: u16 = 28;

#[derive(Debug, Clone)]
pub struct Found {
    pub addresses: Vec<String>,
    pub info: MatterDeviceInfo,
    pub seen: Instant,
}

pub struct Discovery {
    mdns: Arc<MdnsService>,
    operational: Mutex<HashMap<String, Found>>,
    commissionable: Mutex<HashMap<String, Found>>,
    /// Instances dont la résolution directe est en cours.
    resolving: Mutex<HashSet<String>>,
    notify: Notify,
}

impl Discovery {
    pub async fn start() -> Result<Arc<Self>> {
        let (mdns, mut rx) = MdnsService::new().await?;
        let this = Arc::new(Self {
            mdns,
            operational: Mutex::new(HashMap::new()),
            commissionable: Mutex::new(HashMap::new()),
            resolving: Mutex::new(HashSet::new()),
            notify: Notify::new(),
        });
        let weak = Arc::downgrade(&this);
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let Some(this) = weak.upgrade() else { break };
                if let MdnsEvent::ServiceDiscovered { name, target, .. } = event {
                    this.record(&name, &target).await;
                }
            }
        });
        Ok(this)
    }

    async fn record(self: &Arc<Self>, service: &str, target: &str) {
        let service = service.trim_end_matches('.');
        if service != OPERATIONAL && service != COMMISSIONABLE {
            return;
        }
        if self.try_extract(service, target).await {
            return;
        }
        // Les appareils Thread sont publiés par le proxy SRP des routeurs de bordure :
        // la réponse au parcours ne contient souvent que le PTR. On interroge alors
        // l'instance elle-même pour obtenir SRV, TXT et AAAA.
        if !self.resolving.lock().await.insert(target.to_string()) {
            return;
        }
        let this = self.clone();
        let (service, target) = (service.to_string(), target.to_string());
        tokio::spawn(async move {
            for _ in 0..6 {
                this.mdns.active_lookup(&target, QTYPE_ANY).await;
                // L'adresse (AAAA) de l'hôte n'accompagne pas toujours la réponse SRV.
                if let Some(host) = this.host_of(&service, &target).await {
                    this.mdns.active_lookup(&host, QTYPE_AAAA).await;
                }
                tokio::time::sleep(Duration::from_millis(800)).await;
                if this.try_extract(&service, &target).await {
                    break;
                }
            }
            this.resolving.lock().await.remove(&target);
        });
    }

    /// Nom d'hôte (cible SRV) déjà connu pour cette instance.
    async fn host_of(&self, service: &str, target: &str) -> Option<String> {
        let map = if service == OPERATIONAL {
            &self.operational
        } else {
            &self.commissionable
        };
        let map = map.lock().await;
        let device = &map.get(&instance_key(target))?.info.device;
        (!device.is_empty()).then(|| format!("{device}."))
    }

    /// Lit SRV/TXT/AAAA depuis le cache mDNS. `true` seulement si une adresse IP est connue.
    async fn try_extract(&self, service: &str, target: &str) -> bool {
        let map = if service == OPERATIONAL {
            &self.operational
        } else {
            &self.commissionable
        };
        match extract_matter_info(target, &self.mdns).await {
            Ok(info) => {
                let found = to_found(info);
                log::debug!(
                    "mDNS {service}: {} {:?}",
                    found.info.instance,
                    found.addresses
                );
                let complete = !found.addresses.is_empty();
                map.lock().await.insert(instance_key(target), found);
                self.notify.notify_waiters();
                complete
            }
            Err(e) => {
                log::debug!("mDNS {target}: {e}");
                false
            }
        }
    }

    /// Cherche l'adresse d'un nœud opérationnel (`<fabric compressée>-<node id>`).
    pub async fn find_operational(&self, instance: &str, timeout: Duration) -> Result<Found> {
        let key = instance.to_ascii_uppercase();
        let full = format!("{instance}.{OPERATIONAL}");
        let deadline = Instant::now() + timeout;
        let mut last_query = None::<Instant>;
        loop {
            if let Some(f) = self.operational.lock().await.get(&key) {
                if f.seen.elapsed() < Duration::from_secs(120) && !f.addresses.is_empty() {
                    return Ok(f.clone());
                }
            }
            // Les réponses à une requête directe sur l'instance arrivent dans le cache
            // sans forcément générer d'événement « ServiceDiscovered ».
            if let Ok(info) = extract_matter_info(&format!("{full}."), &self.mdns).await {
                let found = to_found(info);
                if !found.addresses.is_empty() {
                    self.operational.lock().await.insert(key, found.clone());
                    return Ok(found);
                }
            }
            if Instant::now() >= deadline {
                bail!("introuvable en mDNS ({instance})");
            }
            if last_query.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) {
                self.mdns.active_lookup(&full, QTYPE_ANY).await;
                self.mdns.active_lookup(OPERATIONAL, QTYPE_ANY).await;
                last_query = Some(Instant::now());
            }
            let _ = tokio::time::timeout(Duration::from_millis(500), self.notify.notified()).await;
        }
    }

    /// Attend un appareil en mode appairage dont le discriminateur correspond au code.
    pub async fn find_commissionable(&self, setup: &SetupInfo, timeout: Duration) -> Result<Found> {
        // Un appareil « sleepy » peut mettre du temps à publier sa fenêtre d'appairage
        // (via le proxy SRP du routeur de bordure) : on réinterroge régulièrement et on
        // n'accepte qu'une annonce récente avec CM ≠ 0 (fenêtre ouverte).
        let deadline = Instant::now() + timeout;
        let mut last_query = None::<Instant>;
        loop {
            {
                let map = self.commissionable.lock().await;
                let hit = map.values().find(|f| {
                    f.seen.elapsed() < Duration::from_secs(90)
                        && !f.addresses.is_empty()
                        && !matches!(f.info.commissioning_mode, Some(CommissioningMode::No))
                        && f.info
                            .discriminator
                            .as_deref()
                            .and_then(|d| d.parse::<u16>().ok())
                            .is_some_and(|d| setup.matches(d))
                });
                if let Some(f) = hit {
                    return Ok(f.clone());
                }
            }
            if Instant::now() >= deadline {
                bail!("aucun appareil en mode appairage ne correspond à ce code");
            }
            if last_query.is_none_or(|t| t.elapsed() > Duration::from_secs(3)) {
                self.mdns.active_lookup(COMMISSIONABLE, QTYPE_ANY).await;
                // Réinterroge aussi les instances déjà connues : leur TXT (CM, D) change
                // quand la fenêtre s'ouvre, sans forcément produire un nouveau PTR.
                let known: Vec<String> = self
                    .commissionable
                    .lock()
                    .await
                    .values()
                    .map(|f| format!("{}.", f.info.instance))
                    .collect();
                for target in known {
                    self.mdns.active_lookup(&target, QTYPE_ANY).await;
                    self.try_extract(COMMISSIONABLE, &target).await;
                }
                last_query = Some(Instant::now());
            }
            let _ = tokio::time::timeout(Duration::from_millis(500), self.notify.notified()).await;
        }
    }

    /// Appareils annoncés comme appairables récemment, quel que soit leur discriminateur
    /// ou leur indicateur CM (repli de l'appairage).
    pub async fn recent_commissionable(&self) -> Vec<Found> {
        self.commissionable
            .lock()
            .await
            .values()
            .filter(|f| f.seen.elapsed() < Duration::from_secs(120) && !f.addresses.is_empty())
            .cloned()
            .collect()
    }

    /// Nœuds opérationnels vus (toutes fabrics confondues), triés : diagnostic.
    pub async fn operational_snapshot(&self) -> Vec<(String, Found)> {
        let mut v: Vec<_> = self
            .operational
            .lock()
            .await
            .iter()
            .map(|(k, f)| (k.clone(), f.clone()))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    pub async fn browse_operational(&self) {
        self.mdns.active_lookup(OPERATIONAL, QTYPE_ANY).await;
    }
}

fn instance_key(target: &str) -> String {
    target
        .split('.')
        .next()
        .unwrap_or(target)
        .to_ascii_uppercase()
}

fn to_found(info: MatterDeviceInfo) -> Found {
    let port = info.port.unwrap_or(5540);
    let addresses = sort_ips(&info.ips)
        .into_iter()
        .map(|ip| match ip {
            IpAddr::V6(v6) => format!("[{v6}]:{port}"),
            IpAddr::V4(v4) => format!("{v4}:{port}"),
        })
        .collect();
    Found {
        addresses,
        info,
        seen: Instant::now(),
    }
}

/// Ordre d'essai : IPv6 globale, puis ULA (fd..), puis IPv4. Les adresses
/// lien-local (fe80::) sont écartées : sans identifiant d'interface, elles
/// ne sont pas utilisables, et un appareil Thread n'en publie pas d'utile.
pub fn sort_ips(ips: &[IpAddr]) -> Vec<IpAddr> {
    fn rank(ip: &IpAddr) -> Option<u8> {
        match ip {
            IpAddr::V6(v6) => {
                let first = v6.segments()[0];
                if first & 0xffc0 == 0xfe80 || v6.is_loopback() || v6.is_unspecified() {
                    None
                } else if first & 0xfe00 == 0xfc00 {
                    Some(1)
                } else {
                    Some(0)
                }
            }
            IpAddr::V4(v4) if v4.is_link_local() || v4.is_loopback() => None,
            IpAddr::V4(_) => Some(2),
        }
    }
    let mut v: Vec<(u8, IpAddr)> = ips
        .iter()
        .filter_map(|ip| rank(ip).map(|r| (r, *ip)))
        .collect();
    v.sort_by_key(|(r, _)| *r);
    v.dedup_by_key(|(_, ip)| *ip);
    v.into_iter().map(|(_, ip)| ip).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_ordering() {
        let ips: Vec<IpAddr> = ["fe80::1", "192.168.1.5", "fd12::5", "2a01:cb00::7"]
            .iter()
            .map(|s| s.parse().unwrap())
            .collect();
        let sorted: Vec<String> = sort_ips(&ips).iter().map(|i| i.to_string()).collect();
        assert_eq!(sorted, vec!["2a01:cb00::7", "fd12::5", "192.168.1.5"]);
    }

    #[test]
    fn instance_key_is_first_label() {
        assert_eq!(
            instance_key("0123456789abcdef-000000000000002a._matter._tcp.local."),
            "0123456789ABCDEF-000000000000002A"
        );
    }
}
