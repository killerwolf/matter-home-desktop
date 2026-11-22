//! Ostal Server s'annonce sur le réseau de la maison (Bonjour, service `_ostal._tcp`) : l'app Mac
//! le trouve à sa première ouverture, sans adresse à taper. L'annonce passe par l'API publique de
//! `matc` (`mdns2::MdnsService`, déjà utilisée pour les appareils Matter) : ni nouvelle crate, ni
//! sous-processus qui pourrait rester orphelin.
//!
//! Ce que l'annonce dit : un nom (celui du Mac), un port, les adresses du réseau de la maison
//! (jamais celle du tailnet) et, en TXT, la version du protocole et l'empreinte du certificat. Rien
//! de secret, et rien ne s'y fie (voir `protocol::discovery`). **Pas d'annonce sur la boucle
//! locale** : un serveur de démonstration ou d'essai (`--listen 127.0.0.1:…`) ne s'annonce jamais.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use matc::mdns2::{MdnsService, ServiceRegistration};
use protocol::discovery;

/// Durée de vie des enregistrements, en secondes. L'annonce est répétée bien avant.
const TTL_S: u32 = 120;
/// L'annonce est rafraîchie à cette cadence (et les adresses relues : le Wi-Fi a pu changer).
const REFRESH_EVERY: Duration = Duration::from_secs(45);

/// Une annonce en cours. L'arrêt (`stop`, ou la destruction) envoie « au revoir » : le serveur
/// disparaît de la liste des apps tout de suite.
pub struct Announcement {
    runtime: Option<tokio::runtime::Runtime>,
    service: Arc<MdnsService>,
    instance: String,
}

/// Le serveur doit-il s'annoncer ? Pas sur la boucle locale.
pub fn should_announce(listen: SocketAddr) -> bool {
    !listen.ip().is_loopback()
}

/// Les adresses du réseau de la maison à annoncer : celle de l'écoute si elle est précise, sinon
/// celles des interfaces. Jamais la boucle locale, le lien local ni le tailnet.
pub fn home_addresses(listen: SocketAddr, interfaces: &[IpAddr]) -> Vec<Ipv4Addr> {
    let mut out: Vec<Ipv4Addr> = Vec::new();
    for ip in interfaces {
        let IpAddr::V4(v4) = ip else { continue };
        let wanted = listen.ip().is_unspecified() || listen.ip() == *ip;
        if wanted && discovery::is_home_lan(*v4) && !out.contains(v4) {
            out.push(*v4);
        }
    }
    out
}

/// Ce qu'on enregistre auprès du service mDNS.
pub fn registration(
    instance: &str,
    port: u16,
    fingerprint: &str,
    addresses: Vec<Ipv4Addr>,
) -> ServiceRegistration {
    ServiceRegistration {
        service_type: discovery::SERVICE.to_string(),
        instance_name: instance.to_string(),
        port,
        hostname: discovery::announced_host(fingerprint),
        txt_records: discovery::txt_records(protocol::wire::PROTOCOL_VERSION, fingerprint),
        ttl: TTL_S,
        subtypes: Vec::new(),
        ips_v4: Some(addresses),
        // Pas d'IPv6 : les adresses temporaires changent sans cesse, l'IPv4 suffit à la maison.
        ips_v6: Some(Vec::new()),
    }
}

fn interfaces() -> Vec<IpAddr> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .iter()
        .map(|i| i.ip())
        .collect()
}

/// Le nom du Mac (« Mac de la maison ») : `scutil --get ComputerName`, une lecture seule.
fn computer_name() -> String {
    std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default()
}

/// Démarre l'annonce, ou `None` : écoute sur la boucle locale, aucune adresse de la maison, ou
/// service mDNS impossible à ouvrir (le serveur marche très bien sans : on le journalise).
pub fn start(listen: SocketAddr, fingerprint: &str) -> Option<Announcement> {
    if !should_announce(listen) {
        return None;
    }
    let addresses = home_addresses(listen, &interfaces());
    if addresses.is_empty() {
        log::info!("pas d'annonce Bonjour : aucune adresse du réseau de la maison");
        return None;
    }
    let instance = discovery::instance_name(&computer_name());
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("annonce")
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            log::warn!("annonce Bonjour impossible (runtime) : {e}");
            return None;
        }
    };
    let (service, _events) = match runtime.block_on(MdnsService::new()) {
        Ok(pair) => pair,
        Err(e) => {
            log::warn!("annonce Bonjour impossible : {e:#}");
            return None;
        }
    };
    let port = listen.port();
    let fp = fingerprint.to_string();
    {
        let (service, instance, addresses) = (service.clone(), instance.clone(), addresses.clone());
        runtime.block_on(async {
            service
                .register_service(registration(&instance, port, &fp, addresses))
                .await;
            service.announce().await;
        });
    }
    // Répète l'annonce, et la refait si les adresses de la maison ont changé.
    {
        let (service, instance) = (service.clone(), instance.clone());
        runtime.spawn(async move {
            let mut current = addresses;
            loop {
                tokio::time::sleep(REFRESH_EVERY).await;
                let now = home_addresses(listen, &interfaces());
                if now.is_empty() {
                    continue;
                }
                if now != current {
                    service
                        .unregister_service(&instance, discovery::SERVICE)
                        .await;
                    service
                        .register_service(registration(&instance, port, &fp, now.clone()))
                        .await;
                    current = now;
                }
                service.announce().await;
            }
        });
    }
    log::info!("annoncé sur le réseau de la maison : « {instance} » (Bonjour, _ostal._tcp)");
    Some(Announcement {
        runtime: Some(runtime),
        service,
        instance,
    })
}

impl Announcement {
    /// Le nom sous lequel le serveur s'annonce.
    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// Dit « au revoir » (le serveur sort aussitôt des listes) et libère le service.
    pub fn stop(mut self) {
        self.close();
    }

    fn close(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            let service = self.service.clone();
            let instance = self.instance.clone();
            runtime.block_on(async move {
                service
                    .unregister_service(&instance, discovery::SERVICE)
                    .await;
                // Laisse partir le paquet d'au revoir avant de couper les sockets.
                tokio::time::sleep(Duration::from_millis(150)).await;
                service.shutdown();
            });
            runtime.shutdown_timeout(Duration::from_secs(1));
        }
    }
}

impl Drop for Announcement {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_and_test_servers_on_the_loopback_never_announce() {
        assert!(!should_announce("127.0.0.1:0".parse().unwrap()));
        assert!(!should_announce("[::1]:8443".parse().unwrap()));
        assert!(should_announce("0.0.0.0:8443".parse().unwrap()));
        assert!(should_announce("192.168.1.20:8443".parse().unwrap()));
        // Et `start` ne touche à rien : pas de runtime, pas de socket.
        assert!(start("127.0.0.1:0".parse().unwrap(), "AA").is_none());
    }

    #[test]
    fn only_home_network_addresses_are_announced_never_the_tailnet() {
        let all: Vec<IpAddr> = [
            "127.0.0.1",
            "192.168.1.20",
            "100.101.102.103",
            "10.0.0.5",
            "169.254.1.2",
            "fe80::1",
        ]
        .iter()
        .map(|a| a.parse().unwrap())
        .collect();
        let any: SocketAddr = "0.0.0.0:8443".parse().unwrap();
        assert_eq!(
            home_addresses(any, &all),
            vec![
                "192.168.1.20".parse::<Ipv4Addr>().unwrap(),
                "10.0.0.5".parse().unwrap()
            ]
        );
        // Écoute sur une adresse précise : seulement celle-là.
        let lan: SocketAddr = "192.168.1.20:8443".parse().unwrap();
        assert_eq!(
            home_addresses(lan, &all),
            vec!["192.168.1.20".parse::<Ipv4Addr>().unwrap()]
        );
        // Écoute sur le tailnet seulement : rien à annoncer sur la maison.
        let ts: SocketAddr = "100.101.102.103:8443".parse().unwrap();
        assert!(home_addresses(ts, &all).is_empty());
    }

    #[test]
    fn the_registration_carries_port_name_txt_and_only_the_given_addresses() {
        let fp = "AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99";
        let reg = registration(
            "Mac de la maison",
            8443,
            fp,
            vec!["192.168.1.20".parse().unwrap()],
        );
        assert_eq!(reg.service_type, "_ostal._tcp.local");
        assert_eq!(
            (reg.instance_name.as_str(), reg.port),
            ("Mac de la maison", 8443)
        );
        assert_eq!(reg.hostname, "ostal-aabbccdd.local");
        assert_eq!(
            reg.ips_v4.as_deref(),
            Some(&["192.168.1.20".parse().unwrap()][..])
        );
        assert_eq!(reg.ips_v6.as_deref(), Some(&[][..]));
        let txt = discovery::parse_txt(
            &reg.txt_records
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>(),
        );
        assert_eq!(txt.protocol, Some(protocol::wire::PROTOCOL_VERSION));
        assert_eq!(txt.fingerprint.as_deref(), Some(fp));
    }

    /// Essai réel : l'annonce d'`start` est vue par un second service mDNS, puis disparaît à
    /// l'arrêt. Envoie pendant quelques secondes une annonce sur le réseau local : à lancer à la
    /// main (`cargo --offline test -p ostal-server --lib announce -- --ignored`).
    #[test]
    #[ignore = "envoie une annonce sur le réseau local"]
    fn the_real_announcement_is_seen_on_the_network_and_goes_away() {
        use matc::mdns::{RRData, TYPE_PTR, TYPE_SRV};
        let fp = "AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99";
        let announcement = start("0.0.0.0:18444".parse().unwrap(), fp)
            .expect("une adresse du réseau de la maison est nécessaire");
        let instance = format!("{}.{}.", announcement.instance(), discovery::SERVICE);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let seen = runtime.block_on(async {
            let (b, _rx) = MdnsService::new().await.unwrap();
            b.add_query(discovery::SERVICE, TYPE_PTR, Duration::from_secs(1))
                .await;
            let mut port = None;
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_millis(300)).await;
                let ptrs = b
                    .lookup(&format!("{}.", discovery::SERVICE), TYPE_PTR)
                    .await;
                let ours = ptrs.iter().any(
                    |r| matches!(&r.data, RRData::PTR(n) if n.eq_ignore_ascii_case(&instance)),
                );
                if ours {
                    for rr in b.lookup(&instance, TYPE_SRV).await {
                        if let RRData::SRV { port: p, .. } = rr.data {
                            port = Some(p);
                        }
                    }
                    if port.is_some() {
                        break;
                    }
                }
            }
            b.shutdown();
            port
        });
        announcement.stop();
        assert_eq!(seen, Some(18444));
    }
}
