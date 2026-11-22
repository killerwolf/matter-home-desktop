//! Trouver Ostal Server sur le réseau de la maison (Bonjour, service `_ostal._tcp`), sans adresse
//! à taper. La recherche passe par l'API publique de `matc` (`mdns2::MdnsService`, la même que
//! celle dont le serveur se sert pour s'annoncer) : ni nouvelle crate, ni sous-processus. Le
//! réseau reste dans un fil à part, avec son petit runtime ; l'interface ne lit que la liste.
//!
//! Une annonce n'est qu'une adresse proposée : l'app s'y connecte comme à une adresse saisie à la
//! main, et le certificat se confirme par son empreinte à la première connexion (voir `tls.rs`).

use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use matc::mdns::{RRData, QTYPE_ANY, TYPE_A, TYPE_PTR, TYPE_SRV, TYPE_TXT};
use matc::mdns2::MdnsService;
use protocol::discovery;

use super::addr::ServerAddr;
use super::Notifier;

/// La recherche s'arrête d'elle-même au bout de ce temps (« Chercher à nouveau » la relance).
const SEARCH_FOR: Duration = Duration::from_secs(90);
/// Cadence de relecture du cache, et de la question répétée sur le réseau.
const POLL: Duration = Duration::from_millis(400);
const ASK_EVERY: Duration = Duration::from_secs(3);

/// Un serveur trouvé.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Le nom du Mac qui l'héberge (« Mac de la maison »).
    pub name: String,
    pub addr: ServerAddr,
    /// L'empreinte annoncée du certificat, si le serveur la dit.
    pub fingerprint: Option<String>,
}

impl Found {
    pub fn url(&self) -> String {
        self.addr.url()
    }
}

/// La recherche en cours. Détruite, elle s'arrête.
pub struct Finder {
    found: Arc<Mutex<Vec<Found>>>,
    searching: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl Finder {
    /// Lance la recherche. `notify` réveille l'interface quand la liste change.
    pub fn start(notify: Notifier) -> Finder {
        let found = Arc::new(Mutex::new(Vec::new()));
        let searching = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let (f, s, st) = (found.clone(), searching.clone(), stop.clone());
        let spawned = std::thread::Builder::new()
            .name("ostal-bonjour".into())
            .spawn(move || {
                if let Err(e) = search(&f, &st, &notify) {
                    log::info!("recherche du serveur impossible : {e}");
                }
                s.store(false, Ordering::SeqCst);
                notify();
            });
        if spawned.is_err() {
            searching.store(false, Ordering::SeqCst);
        }
        Finder {
            found,
            searching,
            stop,
        }
    }

    /// Les serveurs trouvés jusqu'ici, dans l'ordre de leur nom.
    pub fn found(&self) -> Vec<Found> {
        self.found
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// La recherche tourne encore.
    pub fn searching(&self) -> bool {
        self.searching.load(Ordering::SeqCst)
    }
}

impl Drop for Finder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn search(found: &Mutex<Vec<Found>>, stop: &AtomicBool, notify: &Notifier) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let (service, _events) = MdnsService::new().await.map_err(|e| format!("{e:#}"))?;
        service
            .add_query(discovery::SERVICE, TYPE_PTR, ASK_EVERY)
            .await;
        let until = Instant::now() + SEARCH_FOR;
        while !stop.load(Ordering::SeqCst) && Instant::now() < until {
            let list = collect(&service).await;
            {
                let mut shared = found.lock().unwrap_or_else(PoisonError::into_inner);
                if *shared != list {
                    *shared = list;
                    notify();
                }
            }
            tokio::time::sleep(POLL).await;
        }
        service.shutdown();
        Ok(())
    })
}

/// Lit le cache mDNS : pour chaque instance annoncée, son port (SRV), ses adresses (A) et son TXT.
async fn collect(service: &MdnsService) -> Vec<Found> {
    let mut out: Vec<Found> = Vec::new();
    for ptr in service.lookup(&dotted(discovery::SERVICE), TYPE_PTR).await {
        let RRData::PTR(instance) = ptr.data else {
            continue;
        };
        let instance = instance.trim_end_matches('.').to_string();
        let mut srv = None;
        for rr in service.lookup(&dotted(&instance), TYPE_SRV).await {
            if let RRData::SRV { port, target, .. } = rr.data {
                srv = Some((port, target.trim_end_matches('.').to_string()));
            }
        }
        let Some((port, target)) = srv else {
            // Le PTR est arrivé seul : on demande le reste à l'instance elle-même.
            service.active_lookup(&instance, QTYPE_ANY).await;
            continue;
        };
        let mut address: Option<Ipv4Addr> = None;
        for rr in service.lookup(&dotted(&target), TYPE_A).await {
            if let RRData::A(ip) = rr.data {
                if discovery::is_home_lan(ip) && address.is_none() {
                    address = Some(ip);
                }
            }
        }
        let Some(ip) = address else {
            service.active_lookup(&target, TYPE_A).await;
            continue;
        };
        let mut entries: Vec<String> = Vec::new();
        for rr in service.lookup(&dotted(&instance), TYPE_TXT).await {
            // Le contenu brut : le découpage de `matc` ne suit pas les longueurs du TXT.
            entries.extend(discovery::txt_entries(&rr.rdata));
        }
        let name = instance_label(&instance);
        let Ok(addr) = ServerAddr::parse(&format!("https://{ip}:{port}")) else {
            continue;
        };
        let found = Found {
            name,
            addr,
            fingerprint: discovery::parse_txt(&entries).fingerprint,
        };
        if !out.iter().any(|f| f.addr == found.addr) {
            out.push(found);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name).then(a.url().cmp(&b.url())));
    out
}

/// Les noms du cache mDNS portent leur point final (`….local.`) : c'est ainsi qu'on les cherche.
fn dotted(name: &str) -> String {
    format!("{}.", name.trim_end_matches('.'))
}

/// « Mac de la maison._ostal._tcp.local » → « Mac de la maison ».
fn instance_label(instance: &str) -> String {
    let suffix = format!(".{}", discovery::SERVICE);
    let lower = instance.to_lowercase();
    let end = if lower.ends_with(&suffix) {
        instance.len() - suffix.len()
    } else {
        instance.len()
    };
    instance[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_names_are_looked_up_with_their_final_dot() {
        assert_eq!(dotted("_ostal._tcp.local"), "_ostal._tcp.local.");
        assert_eq!(dotted("a.local."), "a.local.");
    }

    #[test]
    fn the_name_is_the_instance_without_the_service() {
        assert_eq!(
            instance_label("Mac de la maison._ostal._tcp.local"),
            "Mac de la maison"
        );
        assert_eq!(instance_label("Mac._OSTAL._tcp.LOCAL"), "Mac");
        assert_eq!(instance_label("Sans suffixe"), "Sans suffixe");
    }
}
