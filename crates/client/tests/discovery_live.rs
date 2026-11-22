//! Essai réel de la découverte : une annonce Bonjour (même forme que celle d'Ostal Server) et la
//! recherche du client, sur le vrai réseau local (multicast). Ignoré par défaut : il envoie pendant
//! quelques secondes une annonce « Ostal essai » sur le réseau de la maison. Lancement à la main :
//! `cargo --offline test -p ostal --test discovery_live -- --ignored`.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use matc::mdns2::{MdnsService, ServiceRegistration};
use ostal_client::net::discovery::Finder;
use protocol::discovery;

const FP: &str = "AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99";

fn home_ip() -> Option<Ipv4Addr> {
    // Une interface du réseau de la maison, lue sans rien envoyer.
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("192.168.255.255:9")
        .ok()
        .or_else(|| sock.connect("10.255.255.255:9").ok())?;
    match sock.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if discovery::is_home_lan(ip) => Some(ip),
        _ => None,
    }
}

#[test]
#[ignore = "envoie une annonce sur le réseau local"]
fn the_client_finds_a_server_announced_the_way_ostal_server_does() {
    let Some(ip) = home_ip() else {
        eprintln!("pas d'adresse du réseau de la maison : essai sauté");
        return;
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let instance = format!("Ostal essai {}", std::process::id());
    let service = runtime.block_on(async {
        let (service, _rx) = MdnsService::new().await.unwrap();
        service
            .register_service(ServiceRegistration {
                service_type: discovery::SERVICE.into(),
                instance_name: instance.clone(),
                port: 18443,
                hostname: discovery::announced_host(FP),
                txt_records: discovery::txt_records(4, FP),
                ttl: 30,
                subtypes: vec![],
                ips_v4: Some(vec![ip]),
                ips_v6: Some(vec![]),
            })
            .await;
        service.announce().await;
        service
    });
    let finder = Finder::start(Arc::new(|| {}));
    let until = Instant::now() + Duration::from_secs(20);
    let mut seen = None;
    while Instant::now() < until && seen.is_none() {
        seen = finder.found().into_iter().find(|f| f.name == instance);
        std::thread::sleep(Duration::from_millis(300));
    }
    runtime.block_on(async {
        service
            .unregister_service(&instance, discovery::SERVICE)
            .await;
        service.shutdown();
    });
    let found = seen.expect("le serveur annoncé n'a pas été trouvé");
    assert_eq!(found.addr.host, ip.to_string());
    assert_eq!(found.addr.port, 18443);
    assert_eq!(found.fingerprint.as_deref(), Some(FP));
}
