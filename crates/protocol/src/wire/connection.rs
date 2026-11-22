//! Les mots d'une connexion, les mêmes dans Ostal Server et dans l'app Mac (l'app web a les
//! siens, dans `connections.js`) : « App web · iPhone », « Sur place », « Hier, 22:10 ».

use super::api::{ConnectionKind, DeviceClass, Via};

/// « App web · iPhone », « App Mac · ordinateur », « App pas encore à jour » (connexion inconnue).
pub fn kind_label(kind: ConnectionKind, device: DeviceClass, unknown: bool) -> String {
    if unknown {
        return "App pas encore à jour".to_string();
    }
    let app = match kind {
        ConnectionKind::Web => "App web",
        ConnectionKind::Mac => "App Mac",
        ConnectionKind::Other => "App",
    };
    let device = match device {
        DeviceClass::Iphone => "iPhone",
        DeviceClass::Ipad => "iPad",
        DeviceClass::Android => "Android",
        DeviceClass::Computer => "ordinateur",
        DeviceClass::Other => return app.to_string(),
    };
    format!("{app} · {device}")
}

/// « Sur place » ou « À distance (Tailscale) ».
pub fn via_label(via: Via) -> &'static str {
    match via {
        Via::Local => "Sur place",
        Via::Tailscale => "À distance (Tailscale)",
    }
}

/// Un accès sous les 2 minutes est « Maintenant ».
const NOW_WITHIN_S: f64 = 120.0;

/// Le dernier accès, en mots : « Maintenant », « 18:31 » (aujourd'hui), « Hier, 22:10 », sinon
/// « 12/11, 21:04 ». `utc_offset_s` : le décalage de l'heure locale, que donne l'appelant.
pub fn last_seen_label(last_seen: f64, now: f64, utc_offset_s: i64) -> String {
    if now - last_seen < NOW_WITHIN_S {
        return "Maintenant".to_string();
    }
    let local = last_seen as i64 + utc_offset_s;
    let today = (now as i64 + utc_offset_s).div_euclid(86_400);
    let day = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let time = format!("{:02}:{:02}", secs / 3600, secs % 3600 / 60);
    match today - day {
        0 => time,
        1 => format!("Hier, {time}"),
        _ => {
            let (_, month, d) = civil_from_days(day);
            format!("{d:02}/{month:02}, {time}")
        }
    }
}

/// Jour du calendrier grégorien pour un nombre de jours depuis le 1er janvier 1970.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_places_are_said_in_plain_words() {
        use ConnectionKind as K;
        use DeviceClass as D;
        assert_eq!(kind_label(K::Web, D::Iphone, false), "App web · iPhone");
        assert_eq!(kind_label(K::Web, D::Android, false), "App web · Android");
        assert_eq!(
            kind_label(K::Mac, D::Computer, false),
            "App Mac · ordinateur"
        );
        assert_eq!(kind_label(K::Web, D::Other, false), "App web");
        assert_eq!(
            kind_label(K::Other, D::Other, true),
            "App pas encore à jour"
        );
        assert_eq!(via_label(Via::Local), "Sur place");
        assert_eq!(via_label(Via::Tailscale), "À distance (Tailscale)");
    }

    #[test]
    fn the_last_access_reads_like_the_mockups() {
        // 2026-11-22 18:41:00 UTC, heure locale = UTC+1.
        let now = 1_795_372_860.0;
        let off = 3600;
        assert_eq!(last_seen_label(now - 30.0, now, off), "Maintenant");
        assert_eq!(last_seen_label(now - 600.0, now, off), "19:31");
        assert_eq!(last_seen_label(now - 86_400.0, now, off), "Hier, 19:41");
        assert_eq!(
            last_seen_label(now - 10.0 * 86_400.0, now, off),
            "12/11, 19:41"
        );
        // Le décalage compte : passé minuit local, ce n'est plus « aujourd'hui ».
        let midnight = ((now as i64 + off).div_euclid(86_400) * 86_400 - off) as f64;
        let after = midnight + 300.0;
        assert_eq!(last_seen_label(midnight + 60.0, after, off), "00:01");
        assert_eq!(
            last_seen_label(midnight - 3600.0, after, off),
            "Hier, 23:00"
        );
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_088), (2024, 12, 31));
    }
}
