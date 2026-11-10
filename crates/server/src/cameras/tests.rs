use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use std::time::Duration;

use protocol::wire::{
    CameraProblem, CameraStatus, StreamInfo, StreamKind, CAMERA_ID_MIN, MAX_CAMERAS,
};
use url::Url;

use super::*;

const SECRET: &str = "S3cret-mdp-de-camera";
const HOST: &str = "192.0.2.77";

fn draft(name: &str, url: &str) -> CameraDraft {
    CameraDraft {
        name: name.into(),
        url: url.into(),
        ..CameraDraft::default()
    }
}

fn with_creds(mut d: CameraDraft) -> CameraDraft {
    d.username = "admin".into();
    d.password = Some(SECRET.into());
    d
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mh-cam-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// Saisie
// ---------------------------------------------------------------------------

#[test]
fn credentials_pasted_in_the_url_move_to_their_fields() {
    let d = draft(
        "Entrée",
        &format!("rtsp://admin:S3cret%40mdp@{HOST}:554/live/ch00_0#x"),
    );
    let c = d.build(CAMERA_ID_MIN, None).unwrap();
    assert_eq!(c.url, format!("rtsp://{HOST}:554/live/ch00_0"));
    assert_eq!(c.username.as_deref(), Some("admin"));
    // Le `@` encodé est décodé.
    assert_eq!(c.password.as_deref(), Some("S3cret@mdp"));
    assert!(c.check().is_ok());
    // Les deux adresses peuvent porter les mêmes identifiants.
    let mut d = draft("x", &format!("rtsp://a:b@{HOST}/m"));
    d.sub_url = format!("rtsp://a:b@{HOST}/s");
    let c = d.build(CAMERA_ID_MIN, None).unwrap();
    assert_eq!(
        c.sub_url.as_deref(),
        Some(format!("rtsp://{HOST}/s").as_str())
    );
    // Des identifiants différents d'une adresse à l'autre : refus.
    d.sub_url = format!("rtsp://a:autre@{HOST}/s");
    assert!(d.build(CAMERA_ID_MIN, None).is_err());
}

#[test]
fn form_fields_win_over_the_url_and_a_password_needs_a_user() {
    let mut d = with_creds(draft("x", &format!("rtsp://autre:zzz@{HOST}/m")));
    let c = d.build(CAMERA_ID_MIN, None).unwrap();
    assert_eq!(
        (c.username.as_deref(), c.password.as_deref()),
        (Some("admin"), Some(SECRET))
    );
    d.username.clear();
    d.url = format!("rtsp://{HOST}/m");
    let e = d.build(CAMERA_ID_MIN, None).unwrap_err();
    assert!(e.0.contains("identifiant"), "{e}");
    assert!(!e.0.contains(SECRET));
}

#[test]
fn password_keep_set_and_clear_on_edit() {
    let first = with_creds(draft("Salon", &format!("rtsp://{HOST}/m")))
        .build(CAMERA_ID_MIN + 1, None)
        .unwrap();
    let mut edit = draft("Salon 2", &format!("rtsp://{HOST}/m"));
    edit.username = "admin".into();
    // `None` : on garde.
    edit.password = None;
    let kept = edit.build(first.id, Some(&first)).unwrap();
    assert_eq!(kept.password.as_deref(), Some(SECRET));
    assert_eq!(kept.name, "Salon 2");
    // `Some("")` : on efface (et l'identifiant n'est plus exigé s'il est vide aussi).
    edit.password = Some(String::new());
    edit.username.clear();
    let cleared = edit.build(first.id, Some(&first)).unwrap();
    assert_eq!((cleared.username, cleared.password), (None, None));
    // `Some(nouveau)` : on remplace.
    edit.username = "admin".into();
    edit.password = Some("autre".into());
    assert_eq!(
        edit.build(first.id, Some(&first))
            .unwrap()
            .password
            .as_deref(),
        Some("autre")
    );
}

#[test]
fn bad_input_is_refused_without_echoing_it() {
    let secret_url = format!("rtsp://admin:{SECRET}@");
    for (what, bad) in [
        ("http", format!("http://admin:{SECRET}@{HOST}/m")),
        ("rtsps", format!("rtsps://admin:{SECRET}@{HOST}/m")),
        ("sans hôte", secret_url.clone()),
        ("illisible", format!("pas une adresse {SECRET}")),
        (
            "trop longue",
            format!("rtsp://{HOST}/{}{SECRET}", "a".repeat(3000)),
        ),
    ] {
        let e = draft("x", &bad).build(CAMERA_ID_MIN, None).unwrap_err();
        assert!(!e.0.contains(SECRET), "{what} : {e}");
        assert!(!e.0.contains(HOST), "{what} : {e}");
    }
    let empty_name = draft("   ", &format!("rtsp://{HOST}/m"));
    assert!(empty_name.build(CAMERA_ID_MIN, None).is_err());
    assert!(draft("a\nb", &format!("rtsp://{HOST}/m"))
        .build(CAMERA_ID_MIN, None)
        .is_err());
    assert!(
        draft(&"n".repeat(MAX_NAME + 1), &format!("rtsp://{HOST}/m"))
            .build(CAMERA_ID_MIN, None)
            .is_err()
    );
    let mut d = with_creds(draft("x", &format!("rtsp://{HOST}/m")));
    d.password = Some("a\nb".into());
    assert!(d.build(CAMERA_ID_MIN, None).is_err());
}

// ---------------------------------------------------------------------------
// Secrets
// ---------------------------------------------------------------------------

#[test]
fn debug_output_never_shows_an_address_or_a_secret() {
    let camera = with_creds(draft("Entrée", &format!("rtsp://{HOST}:554/live/ch00_0")))
        .build(CAMERA_ID_MIN + 9, None)
        .unwrap();
    let target = camera.target(StreamKind::Main).unwrap();
    let texts = [
        format!("{camera:?}"),
        format!("{:?}", with_creds(draft("n", &format!("rtsp://{HOST}/m")))),
        format!("{target:?}"),
        format!("{:?}", target.credentials),
        format!(
            "{:#?}",
            CameraFile {
                version: 1,
                cameras: vec![camera.clone()]
            }
        ),
        format!("{:?}", CameraBook::new(vec![camera])),
    ];
    for t in texts {
        assert!(!t.contains(SECRET), "{t}");
        assert!(!t.contains(HOST), "{t}");
        assert!(!t.contains("admin"), "{t}");
    }
}

#[test]
fn masked_url_hides_credentials_and_unknown_text() {
    assert_eq!(
        masked_url(&format!("rtsp://admin:{SECRET}@{HOST}:554/live")),
        format!("rtsp://***@{HOST}:554/live")
    );
    assert_eq!(
        masked_url(&format!("rtsp://{HOST}/live")),
        format!("rtsp://{HOST}/live")
    );
    assert_eq!(
        masked_url(&format!("nimporte quoi {SECRET}")),
        "<adresse illisible>"
    );
}

#[test]
fn clients_see_no_address_and_no_secret() {
    let mut book = CameraBook::default();
    let id = book
        .add(&with_creds(draft(
            "Entrée",
            &format!("rtsp://{HOST}:554/live/ch00_0"),
        )))
        .unwrap();
    book.set_health(
        id,
        CameraStatus::Online,
        Some(StreamInfo {
            codec: "h264".into(),
            width: Some(1280),
            height: Some(720),
        }),
    );
    let json = serde_json::to_string(&book.views()).unwrap();
    for needle in [SECRET, HOST, "admin", "rtsp", "live"] {
        assert!(!json.contains(needle), "{needle} dans {json}");
    }
}

// ---------------------------------------------------------------------------
// Carnet
// ---------------------------------------------------------------------------

#[test]
fn book_adds_edits_and_deletes_with_reserved_ids() {
    let mut book = CameraBook::default();
    let a = book
        .add(&draft("Zèbre", &format!("rtsp://{HOST}/a")))
        .unwrap();
    let b = book
        .add(&draft("abeille", &format!("rtsp://{HOST}/b")))
        .unwrap();
    assert!(protocol::wire::is_camera_id(a) && protocol::wire::is_camera_id(b) && a != b);
    // Trié par nom sans tenir compte de la casse.
    let names: Vec<_> = book.views().into_iter().map(|v| v.name).collect();
    assert_eq!(names, ["abeille", "Zèbre"]);
    assert!(book
        .views()
        .iter()
        .all(|v| v.status == CameraStatus::Unknown));

    book.set_health(a, CameraStatus::Online, None);
    // Renommer ne remet pas l'état à zéro ; changer l'adresse, si.
    let mut d = draft("Zèbre 2", &format!("rtsp://{HOST}/a"));
    book.update(a, &d).unwrap();
    assert_eq!(
        book.views().iter().find(|v| v.id == a).unwrap().status,
        CameraStatus::Online
    );
    d.name = "Zèbre 3".into();
    book.update(a, &d).unwrap();
    assert_eq!(
        book.views().iter().find(|v| v.id == a).unwrap().status,
        CameraStatus::Online
    );
    d.url = format!("rtsp://{HOST}/autre");
    book.update(a, &d).unwrap();
    assert_eq!(
        book.views().iter().find(|v| v.id == a).unwrap().status,
        CameraStatus::Unknown
    );

    book.remove(a).unwrap();
    assert!(book.get(a).is_none() && book.update(a, &d).is_err() && book.remove(a).is_err());
    assert_eq!(book.cameras().len(), 1);
}

#[test]
fn book_caps_the_number_of_cameras() {
    let mut book = CameraBook::default();
    for i in 0..MAX_CAMERAS {
        book.add(&draft(&format!("c{i}"), &format!("rtsp://{HOST}/{i}")))
            .unwrap();
    }
    assert!(book
        .add(&draft("de trop", &format!("rtsp://{HOST}/x")))
        .is_err());
}

#[test]
fn a_failed_probe_keeps_the_last_known_stream() {
    let mut book = CameraBook::default();
    let id = book.add(&draft("x", &format!("rtsp://{HOST}/a"))).unwrap();
    let info = StreamInfo {
        codec: "h264".into(),
        width: Some(1280),
        height: Some(720),
    };
    assert!(book.set_health(id, CameraStatus::Online, Some(info.clone())));
    // Même état : rien à diffuser.
    assert!(!book.set_health(id, CameraStatus::Online, Some(info.clone())));
    let down = CameraStatus::Unreachable {
        problem: CameraProblem::Unreachable,
        detail: None,
        since: None,
    };
    assert!(book.set_health(id, down.clone(), None));
    let view = &book.views()[0];
    assert_eq!((&view.status, &view.stream), (&down, &Some(info)));
    assert!(
        !book.set_health(CAMERA_ID_MIN + 123_456, down, None),
        "caméra inconnue"
    );
}

#[test]
fn the_demo_has_a_few_cameras_in_each_state() {
    let views = CameraBook::demo().views();
    assert_eq!(views.len(), 3);
    assert!(views
        .iter()
        .any(|v| v.status == CameraStatus::Online && v.has_sub_stream));
    assert!(views
        .iter()
        .any(|v| v.status == CameraStatus::Online && !v.has_sub_stream));
    assert!(views
        .iter()
        .any(|v| matches!(v.status, CameraStatus::Unreachable { .. })));
    // Les caméras de démonstration passent les mêmes contrôles que les vraies.
    for c in CameraBook::demo().cameras() {
        c.check().unwrap();
    }
}

#[test]
fn preferred_stream_is_the_sub_stream_when_there_is_one() {
    let mut d = draft("x", &format!("rtsp://{HOST}/live/ch00_0"));
    let main_only = d.build(CAMERA_ID_MIN, None).unwrap();
    assert_eq!(main_only.preferred_kind(), StreamKind::Main);
    assert!(main_only.target(StreamKind::Sub).is_none());
    d.sub_url = format!("rtsp://{HOST}/live/ch00_1");
    let both = d.build(CAMERA_ID_MIN, None).unwrap();
    assert_eq!(both.preferred_kind(), StreamKind::Sub);
    assert_eq!(
        both.target(StreamKind::Sub).unwrap().url.path(),
        "/live/ch00_1"
    );
    assert_eq!(
        both.target(StreamKind::Main).unwrap().url.path(),
        "/live/ch00_0"
    );
}

// ---------------------------------------------------------------------------
// Persistance
// ---------------------------------------------------------------------------

#[test]
fn cameras_are_persisted_private_and_atomic() {
    let dir = temp_dir("save");
    let mut book = CameraBook::default();
    let id = book
        .add(&with_creds(draft(
            "Entrée",
            &format!("rtsp://{HOST}:554/live/ch00_0"),
        )))
        .unwrap();
    save(&dir, book.cameras()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join(FILE_NAME))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // Pas de fichier temporaire oublié.
    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, [FILE_NAME]);
    let back = load(&dir);
    assert_eq!(back, book.cameras());
    assert_eq!(back[0].id, id);
    // L'ancien contenu est remplacé d'un coup, droits conservés.
    book.remove(id).unwrap();
    save(&dir, book.cameras()).unwrap();
    assert!(load(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn old_and_foreign_files_still_load() {
    let dir = temp_dir("compat");
    // Sans champs facultatifs, avec un champ inconnu d'une version plus récente (placement…).
    std::fs::write(
        dir.join(FILE_NAME),
        format!(
            r#"{{"version":1,"futur":true,"cameras":[
                {{"id":{CAMERA_ID_MIN},"name":"Garage","url":"rtsp://{HOST}/m","orientation":90}}]}}"#
        ),
    )
    .unwrap();
    let cameras = load(&dir);
    assert_eq!(cameras.len(), 1);
    assert_eq!(cameras[0].sub_url, None);
    assert_eq!(cameras[0].username, None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unreadable_file_is_set_aside_not_overwritten() {
    let dir = temp_dir("bad");
    std::fs::write(dir.join(FILE_NAME), "{ pas du json").unwrap();
    assert!(load(&dir).is_empty());
    assert!(!dir.join(FILE_NAME).exists());
    assert_eq!(
        std::fs::read_to_string(dir.join("cameras.json.illisible")).unwrap(),
        "{ pas du json"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn invalid_duplicate_and_out_of_range_cameras_are_dropped_on_load() {
    let dir = temp_dir("drop");
    let ok = StoredCamera {
        id: CAMERA_ID_MIN + 1,
        name: "Bonne".into(),
        url: format!("rtsp://{HOST}/m"),
        sub_url: None,
        username: None,
        password: None,
    };
    let dup = StoredCamera {
        name: "Double".into(),
        ..ok.clone()
    };
    let out_of_range = StoredCamera {
        id: 5,
        name: "Hors plage".into(),
        ..ok.clone()
    };
    let with_userinfo = StoredCamera {
        id: CAMERA_ID_MIN + 2,
        name: "Identifiants dans l'adresse".into(),
        url: format!("rtsp://a:b@{HOST}/m"),
        ..ok.clone()
    };
    let not_rtsp = StoredCamera {
        id: CAMERA_ID_MIN + 3,
        name: "Web".into(),
        url: "http://exemple.test/".into(),
        ..ok.clone()
    };
    let file = CameraFile {
        version: 1,
        cameras: vec![ok.clone(), dup, out_of_range, with_userinfo, not_rtsp],
    };
    std::fs::write(dir.join(FILE_NAME), serde_json::to_string(&file).unwrap()).unwrap();
    assert_eq!(load(&dir), [ok]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_backup_file_is_validated_strictly() {
    let camera = |n: u64| {
        draft("c", &format!("rtsp://{HOST}/{n}"))
            .build(CAMERA_ID_MIN + n, None)
            .unwrap()
    };
    let good = CameraFile {
        version: 1,
        cameras: vec![camera(1), camera(2)],
    };
    good.validate().unwrap();
    let dup = CameraFile {
        version: 1,
        cameras: vec![camera(1), camera(1)],
    };
    assert!(dup.validate().is_err());
    let future = CameraFile {
        version: 2,
        cameras: vec![],
    };
    assert!(future.validate().is_err());
    let mut with_secret_url = camera(3);
    with_secret_url.url = format!("rtsp://admin:{SECRET}@{HOST}/x");
    let bad = CameraFile {
        version: 1,
        cameras: vec![with_secret_url],
    };
    let e = bad.validate().unwrap_err();
    assert!(!e.0.contains(SECRET) && !e.0.contains(HOST), "{e}");
}

// ---------------------------------------------------------------------------
// Vérification d'un flux, contre une fausse caméra locale (aucun trafic réel)
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum Camera {
    /// Répond au `DESCRIBE` sans authentification.
    Open,
    /// Exige l'authentification Basic (401 d'abord).
    Basic {
        user: &'static str,
        password: &'static str,
    },
    /// 404 sur tout.
    NotFound,
    /// Du texte qui n'est pas du RTSP.
    Garbage,
    /// Accepte la connexion, ne répond jamais.
    Silent,
    /// SDP sans piste vidéo.
    AudioOnly,
    /// Répond au premier `DESCRIBE`, puis 404 (la caméra « disparaît »).
    OpenOnce,
    /// Exige l'authentification Digest (MD5, `qop=auth`), celle que parlent la plupart des
    /// caméras IP.
    Digest {
        user: &'static str,
        password: &'static str,
    },
}

/// Le SPS de cet exemple (de la suite de tests de `retina`) décrit du 704 × 480.
const SDP_H264: &str = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=Fausse camera\r\nt=0 0\r\na=control:*\r\n\
m=audio 0 RTP/AVP 96\r\na=rtpmap:96 mpeg4-generic/12000/2\r\n\
a=fmtp:96 profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3;config=1490\r\n\
a=control:trackID=1\r\n\
m=video 0 RTP/AVP 97\r\na=rtpmap:97 H264/90000\r\n\
a=fmtp:97 packetization-mode=1;profile-level-id=64001E;sprop-parameter-sets=Z2QAHqwsaoLA9puCgIKgAAADACAAAAMD0IAA,aO4xshsA\r\n\
a=control:trackID=2\r\n";

const SDP_AUDIO_ONLY: &str = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=x\r\nt=0 0\r\na=control:*\r\n\
m=audio 0 RTP/AVP 96\r\na=rtpmap:96 mpeg4-generic/12000/2\r\n\
a=fmtp:96 profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3;config=1490\r\n\
a=control:trackID=1\r\n";

/// Lance la fausse caméra. Rend son port et le nombre de requêtes `DESCRIBE` reçues.
async fn fake_camera(kind: Camera) -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let describes = Arc::new(AtomicUsize::new(0));
    let counter = describes.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let (kind, counter) = (kind.clone(), counter.clone());
            tokio::spawn(async move {
                let mut pending = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let Ok(n) = socket.read(&mut chunk).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    pending.extend_from_slice(&chunk[..n]);
                    while let Some(end) = pending.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&pending[..end]).into_owned();
                        pending.drain(..end + 4);
                        let reply = answer(&kind, &head, &counter);
                        let Some(reply) = reply else {
                            // Silencieux : on garde la connexion ouverte sans répondre.
                            continue;
                        };
                        if socket.write_all(reply.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                }
            });
        }
    });
    (port, describes)
}

fn answer(kind: &Camera, head: &str, describes: &AtomicUsize) -> Option<String> {
    let cseq = head
        .lines()
        .find_map(|l| l.strip_prefix("CSeq: "))
        .unwrap_or("1")
        .trim()
        .to_string();
    let reply = |status: &str, extra: &str, body: &str| {
        format!(
            "RTSP/1.0 {status}\r\nCSeq: {cseq}\r\n{extra}Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
    };
    let sdp = |body: &str| {
        reply(
            "200 OK",
            "Content-Type: application/sdp\r\nContent-Base: rtsp://127.0.0.1/live/\r\n",
            body,
        )
    };
    if !head.starts_with("DESCRIBE ") {
        return Some(reply("405 Method Not Allowed", "", ""));
    }
    describes.fetch_add(1, Ordering::SeqCst);
    match kind {
        Camera::Open => Some(sdp(SDP_H264)),
        Camera::OpenOnce => {
            if describes.load(Ordering::SeqCst) == 1 {
                Some(sdp(SDP_H264))
            } else {
                Some(reply("404 Not Found", "", ""))
            }
        }
        Camera::AudioOnly => Some(sdp(SDP_AUDIO_ONLY)),
        Camera::NotFound => Some(reply("404 Not Found", "", "")),
        Camera::Garbage => Some("HTTP/1.1 200 OK\r\n\r\nceci n'est pas du RTSP\r\n\r\n".into()),
        Camera::Silent => None,
        Camera::Digest { user, password } => {
            let challenge = "WWW-Authenticate: Digest realm=\"camera\", nonce=\"dcd98b7102dd2f0e8b11d0f600bfb0c093\", qop=\"auth\", algorithm=MD5\r\n";
            let Some(auth) = head
                .lines()
                .find_map(|l| l.strip_prefix("Authorization: Digest "))
            else {
                return Some(reply("401 Unauthorized", challenge, ""));
            };
            let field = |name: &str| {
                auth.split(',').find_map(|part| {
                    let (k, v) = part.trim().split_once('=')?;
                    (k == name).then(|| v.trim_matches('"').to_string())
                })
            };
            let md5 = |text: String| {
                use md5::Digest;
                hex::encode(md5::Md5::digest(text.as_bytes()))
            };
            let (Some(uri), Some(nonce), Some(nc), Some(cnonce), Some(response)) = (
                field("uri"),
                field("nonce"),
                field("nc"),
                field("cnonce"),
                field("response"),
            ) else {
                return Some(reply("401 Unauthorized", challenge, ""));
            };
            let ha1 = md5(format!("{user}:camera:{password}"));
            let ha2 = md5(format!("DESCRIBE:{uri}"));
            let expected = md5(format!("{ha1}:{nonce}:{nc}:{cnonce}:auth:{ha2}"));
            if field("username").as_deref() == Some(*user) && response == expected {
                Some(sdp(SDP_H264))
            } else {
                Some(reply("401 Unauthorized", challenge, ""))
            }
        }
        Camera::Basic { user, password } => {
            use base64::Engine;
            let expected = format!(
                "Authorization: Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
            );
            if head.lines().any(|l| l == expected) {
                Some(sdp(SDP_H264))
            } else {
                Some(reply(
                    "401 Unauthorized",
                    "WWW-Authenticate: Basic realm=\"camera\"\r\n",
                    "",
                ))
            }
        }
    }
}

fn local_target(port: u16, credentials: Option<(&str, &str)>) -> Target {
    Target {
        url: Url::parse(&format!("rtsp://127.0.0.1:{port}/live/ch00_1")).unwrap(),
        credentials: credentials.map(|(u, p)| Credentials {
            username: u.into(),
            password: p.into(),
        }),
    }
}

const SHORT: Duration = Duration::from_millis(1500);

#[tokio::test]
async fn probe_reads_the_codec_and_the_resolution() {
    let (port, describes) = fake_camera(Camera::Open).await;
    let info = probe(&local_target(port, None), Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(info.codec, "h264");
    assert_eq!((info.width, info.height), (Some(704), Some(480)));
    assert_eq!(describe_stream(&info), "H.264 · 704 × 480");
    assert_eq!(describes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn probe_authenticates_with_the_stored_credentials() {
    let (port, _) = fake_camera(Camera::Basic {
        user: "admin",
        password: SECRET,
    })
    .await;
    let ok = probe(
        &local_target(port, Some(("admin", SECRET))),
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(ok.unwrap().codec, "h264");
    let wrong = probe(
        &local_target(port, Some(("admin", "mauvais"))),
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert_eq!(wrong.problem, CameraProblem::Unauthorized);
    let none = probe(&local_target(port, None), Duration::from_secs(5))
        .await
        .unwrap_err();
    assert_eq!(none.problem, CameraProblem::Unauthorized);
    // Le message d'erreur ne reprend ni le mot de passe ni l'adresse.
    for e in [&wrong, &none] {
        assert!(
            !e.message.contains("mauvais") && !e.message.contains("127.0.0.1"),
            "{e}"
        );
    }
}

#[tokio::test]
async fn probe_classifies_the_failures() {
    let (port, _) = fake_camera(Camera::NotFound).await;
    let e = probe(&local_target(port, None), Duration::from_secs(5))
        .await
        .unwrap_err();
    assert_eq!(e.problem, CameraProblem::NotFound);

    let (port, _) = fake_camera(Camera::Garbage).await;
    let e = probe(&local_target(port, None), Duration::from_secs(5))
        .await
        .unwrap_err();
    assert_eq!(e.problem, CameraProblem::Protocol, "{e}");

    let (port, _) = fake_camera(Camera::AudioOnly).await;
    let e = probe(&local_target(port, None), Duration::from_secs(5))
        .await
        .unwrap_err();
    assert_eq!(e.problem, CameraProblem::Protocol, "{e}");

    // Silence : le délai joue.
    let (port, _) = fake_camera(Camera::Silent).await;
    let started = std::time::Instant::now();
    let e = probe(&local_target(port, None), SHORT).await.unwrap_err();
    assert_eq!(e.problem, CameraProblem::Unreachable);
    assert!(started.elapsed() < Duration::from_secs(5));

    // Port fermé : refusé tout de suite.
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let e = probe(&local_target(closed, None), Duration::from_secs(5))
        .await
        .unwrap_err();
    assert_eq!(e.problem, CameraProblem::Unreachable);
    assert!(!e.message.contains("127.0.0.1") && !e.message.contains(&closed.to_string()));
}

#[test]
fn the_test_button_checks_both_streams_one_after_the_other() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (port, describes) = runtime.block_on(fake_camera(Camera::Open));
    let mut d = with_creds(draft(
        "Entrée",
        &format!("rtsp://127.0.0.1:{port}/live/ch00_0"),
    ));
    d.sub_url = format!("rtsp://127.0.0.1:{port}/live/ch00_1");
    let camera = d.build(CAMERA_ID_MIN, None).unwrap();
    let results = CameraTest::spawn(camera, None).wait().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(describes.load(Ordering::SeqCst), 2);
    assert_eq!(results[0].describe(), "Flux principal : H.264 · 704 × 480");
    assert_eq!(results[1].kind, StreamKind::Sub);
    // Sans flux réduit, un seul test.
    let single = draft("x", &format!("rtsp://127.0.0.1:{port}/m"))
        .build(CAMERA_ID_MIN, None)
        .unwrap();
    assert_eq!(CameraTest::spawn(single, None).wait().unwrap().len(), 1);
}

#[tokio::test]
async fn probe_answers_a_digest_challenge() {
    let (port, describes) = fake_camera(Camera::Digest {
        user: "admin",
        password: SECRET,
    })
    .await;
    let ok = probe(
        &local_target(port, Some(("admin", SECRET))),
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(ok.unwrap().codec, "h264");
    assert!(describes.load(Ordering::SeqCst) >= 2, "défi puis réponse");
    let wrong = probe(
        &local_target(port, Some(("admin", "autre"))),
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert_eq!(wrong.problem, CameraProblem::Unauthorized);
}

// ---------------------------------------------------------------------------
// File d'attente, identifiants, hystérésis, fichiers, formulaire
// ---------------------------------------------------------------------------

#[test]
fn the_queue_coalesces_wakes_and_puts_woken_cameras_first() {
    let mut q = Queue::default();
    q.wake_all([1, 2, 3]);
    // Une demande pour 3 le fait passer devant, une seule fois.
    for _ in 0..5 {
        q.wake_camera(3);
    }
    q.wake_all([1, 2, 3]);
    assert_eq!(
        [q.pop(), q.pop(), q.pop(), q.pop()],
        [Some(3), Some(1), Some(2), None]
    );
    assert!(q.is_empty());
    // Une caméra réveillée deux fois ne revient pas dans le balayage qui la contient déjà.
    q.wake_all([7, 8]);
    q.wake_camera(8);
    q.wake_all([8]);
    assert_eq!([q.pop(), q.pop(), q.pop()], [Some(8), Some(7), None]);
}

#[test]
fn a_fresh_id_skips_taken_and_foreign_ids() {
    let book = CameraBook::default();
    let taken = std::collections::HashSet::from([CAMERA_ID_MIN + 1]);
    let mut draws = [5, u64::MAX, CAMERA_ID_MIN + 1, CAMERA_ID_MIN + 2].into_iter();
    // 5 est hors plage, `u64::MAX` est interdit, `+1` est celui d'un appareil.
    assert_eq!(
        book.fresh_id_with(&taken, || draws.next().unwrap()),
        CAMERA_ID_MIN + 2
    );
}

#[test]
fn a_reachable_camera_needs_two_failures_and_the_reason_decides_what_is_news() {
    let mut book = CameraBook::default();
    let id = book.add(&draft("x", &format!("rtsp://{HOST}/a"))).unwrap();
    let info = StreamInfo {
        codec: "h264".into(),
        width: Some(1),
        height: Some(1),
    };
    let fail = |p: CameraProblem, text: &str| Err((p, text.to_string()));
    // Jamais vue joignable : injoignable dès le premier échec.
    let r = book.record_probe(id, fail(CameraProblem::Unreachable, "a"));
    assert_eq!((r.changed, r.recheck), (true, false));
    book.record_probe(id, Ok(info));
    let r = book.record_probe(id, fail(CameraProblem::Unreachable, "a"));
    assert_eq!((r.changed, r.recheck), (false, true));
    assert_eq!(book.status(id), Some(&CameraStatus::Online));
    // Un succès entre deux échecs remet le compte à zéro.
    book.record_probe(
        id,
        Ok(StreamInfo {
            codec: "h264".into(),
            width: Some(1),
            height: Some(1),
        }),
    );
    let r = book.record_probe(id, fail(CameraProblem::Unreachable, "a"));
    assert!(r.recheck && !r.changed);
    let r = book.record_probe(id, fail(CameraProblem::Unreachable, "b"));
    assert_eq!((r.changed, r.recheck), (true, false));
    // Même motif, autre phrase : pas une nouvelle. Autre motif : oui.
    assert!(
        !book
            .record_probe(id, fail(CameraProblem::Unreachable, "c"))
            .changed
    );
    assert!(
        book.record_probe(id, fail(CameraProblem::Unauthorized, "c"))
            .changed
    );
}

#[test]
fn an_unreadable_file_never_replaces_an_earlier_one_and_dropped_cameras_are_kept() {
    let dir = temp_dir("aside");
    std::fs::write(dir.join(FILE_NAME), "{ un").unwrap();
    assert!(load(&dir).is_empty());
    std::fs::write(dir.join(FILE_NAME), "{ deux").unwrap();
    assert!(load(&dir).is_empty());
    assert_eq!(
        std::fs::read_to_string(dir.join("cameras.json.illisible")).unwrap(),
        "{ un"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("cameras.json.illisible-2")).unwrap(),
        "{ deux"
    );
    // Une caméra écartée au chargement : le fichier d'origine est copié avant qu'un
    // enregistrement ne l'écrase.
    let good = draft("Bonne", &format!("rtsp://{HOST}/a"))
        .build(CAMERA_ID_MIN + 1, None)
        .unwrap();
    let mut bad = good.clone();
    bad.id = CAMERA_ID_MIN + 2;
    bad.url = "http://pas-rtsp/".into();
    let original = serde_json::to_string(&CameraFile {
        version: 1,
        cameras: vec![good.clone(), bad],
    })
    .unwrap();
    std::fs::write(dir.join(FILE_NAME), &original).unwrap();
    assert_eq!(load(&dir), [good]);
    assert_eq!(
        std::fs::read_to_string(dir.join("cameras.json.ecartees")).unwrap(),
        original
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("cameras.json.ecartees"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "la copie n'est lisible que de son propriétaire"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn moving_a_camera_to_another_host_requires_the_password_again() {
    let first = with_creds(draft("Salon", &format!("rtsp://{HOST}/m")))
        .build(CAMERA_ID_MIN + 1, None)
        .unwrap();
    let mut edit = draft("Salon", "rtsp://198.51.100.9/m");
    edit.username = "admin".into();
    edit.password = None;
    let e = edit.build(first.id, Some(&first)).unwrap_err();
    assert!(e.0.contains("saisissez de nouveau le mot de passe"), "{e}");
    assert!(!e.0.contains("198.51.100.9") && !e.0.contains(SECRET));
    // Le même hôte (autre port, autre chemin) garde le mot de passe.
    edit.url = format!("rtsp://{HOST}:8554/autre");
    assert_eq!(
        edit.build(first.id, Some(&first))
            .unwrap()
            .password
            .as_deref(),
        Some(SECRET)
    );
    // Un nouveau mot de passe saisi autorise le changement.
    edit.url = "rtsp://198.51.100.9/m".into();
    edit.password = Some("neuf".into());
    assert_eq!(
        edit.build(first.id, Some(&first))
            .unwrap()
            .password
            .as_deref(),
        Some("neuf")
    );
}

#[test]
fn refusals_start_with_a_capital_and_credentials_can_be_split_for_the_form() {
    let e = draft("", &format!("rtsp://{HOST}/m"))
        .build(CAMERA_ID_MIN, None)
        .unwrap_err();
    assert!(e.0.starts_with("Nom"), "{e}");
    let e = draft("x", "http://a/")
        .build(CAMERA_ID_MIN, None)
        .unwrap_err();
    assert!(e.0.starts_with(|c: char| c.is_uppercase()), "{e}");
    let (clean, (user, password)) =
        split_credentials(&format!("rtsp://admin:p%40ss@{HOST}:554/live/ch00_1")).unwrap();
    assert_eq!(clean, format!("rtsp://{HOST}:554/live/ch00_1"));
    assert_eq!(
        (user.as_str(), password.as_deref()),
        ("admin", Some("p@ss"))
    );
    assert!(split_credentials(&format!("rtsp://{HOST}/m")).is_none());
    assert!(split_credentials("n'importe quoi").is_none());
}

// ---------------------------------------------------------------------------
// Test annulable et boucle de surveillance (fausses caméras locales)
// ---------------------------------------------------------------------------

#[test]
fn a_cancelled_test_stops_at_once_and_gives_the_camera_lock_back() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (port, _) = runtime.block_on(fake_camera(Camera::Silent));
    let camera = draft("Muette", &format!("rtsp://127.0.0.1:{port}/m"))
        .build(CAMERA_ID_MIN, None)
        .unwrap();
    let lock = std::sync::Arc::new(std::sync::Mutex::new(()));
    let test = CameraTest::spawn(camera.clone(), Some(lock.clone()));
    std::thread::sleep(Duration::from_millis(300));
    let started = std::time::Instant::now();
    test.cancel();
    let e = test.wait().unwrap_err();
    assert_eq!(e.0, "Test annulé.");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "annulé tout de suite, pas après 8 s"
    );
    assert!(lock.try_lock().is_ok(), "le verrou est rendu");
    // Attendre le verrou d'un autre se termine aussi sur annulation.
    let held = lock.lock().unwrap();
    let waiting = CameraTest::spawn(camera, Some(lock.clone()));
    std::thread::sleep(Duration::from_millis(200));
    assert!(waiting.poll().is_none(), "toujours en attente du verrou");
    waiting.cancel();
    assert_eq!(waiting.wait().unwrap_err().0, "Test annulé.");
    drop(held);
}

fn hub_with_camera(
    tag: &str,
    port: u16,
) -> (std::sync::Arc<crate::hub::Hub>, u64, std::path::PathBuf) {
    let dir = temp_dir(tag);
    let (hub, _rx) = crate::hub::tests::hub_with(&dir, 16);
    let hub = std::sync::Arc::new(hub);
    let id = hub
        .add_camera(&draft(
            "Locale",
            &format!("rtsp://127.0.0.1:{port}/live/ch00_1"),
        ))
        .unwrap();
    (hub, id, dir)
}

/// Fait tourner la boucle de surveillance avec ces demandes pendant `for_how_long`, puis l'arrête.
/// (La boucle n'est pas `Send` : elle tourne ici, dans la tâche du test, avec le pilote.)
async fn run_monitor(hub: &crate::hub::Hub, wakes: Vec<Wake>, for_how_long: Duration) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    for w in wakes {
        tx.send(w).unwrap();
    }
    let stop = tokio::sync::Notify::new();
    let driver = async {
        tokio::time::sleep(for_how_long).await;
        stop.notify_one();
    };
    let hour = Duration::from_secs(3600);
    tokio::join!(monitor_loop(hub, rx, &stop, hour, hour), driver);
}

#[tokio::test]
async fn the_monitor_stops_in_the_middle_of_a_probe_to_a_silent_camera() {
    let (port, describes) = fake_camera(Camera::Silent).await;
    let (hub, id, dir) = hub_with_camera("mon-stop", port);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tx.send(Wake::Camera(id)).unwrap();
    let stop = tokio::sync::Notify::new();
    let hour = Duration::from_secs(3600);
    let started = std::time::Instant::now();
    let driver = async {
        // La vérification est en cours (la caméra a reçu son DESCRIBE et ne répond pas)…
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while describes.load(Ordering::SeqCst) == 0 {
            assert!(std::time::Instant::now() < deadline, "aucune vérification");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // … et l'arrêt ne l'attend pas.
        stop.notify_one();
    };
    tokio::time::timeout(Duration::from_secs(4), async {
        tokio::join!(monitor_loop(&hub, rx, &stop, hour, hour), driver)
    })
    .await
    .expect("arrêt immédiat");
    // Le délai d'une vérification est de 8 s : on n'a pas attendu la fin de celle-ci.
    assert!(started.elapsed() < Duration::from_secs(4));
    assert_eq!(
        hub.cameras()[0].status,
        CameraStatus::Unknown,
        "rien n'est enregistré"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // le verrou tenu pendant `run_monitor` est le but du test
async fn the_monitor_coalesces_wakes_and_skips_watched_or_locked_cameras() {
    let (port, describes) = fake_camera(Camera::Open).await;
    let (hub, id, dir) = hub_with_camera("mon-wake", port);
    let half = Duration::from_millis(700);
    // Cinq demandes d'affilée pour la même caméra : une seule vérification.
    run_monitor(&hub, vec![Wake::Camera(id); 5], half).await;
    assert_eq!(describes.load(Ordering::SeqCst), 1);
    assert_eq!(hub.cameras()[0].status, CameraStatus::Online);
    // Un balayage complet mêlé de demandes pour la même caméra : encore une seule.
    run_monitor(&hub, vec![Wake::All, Wake::Camera(id), Wake::All], half).await;
    assert_eq!(describes.load(Ordering::SeqCst), 2);
    // Caméra regardée : sautée.
    let lease = hub.acquire_camera_viewer(id).unwrap();
    run_monitor(&hub, vec![Wake::Camera(id)], half).await;
    assert_eq!(describes.load(Ordering::SeqCst), 2);
    drop(lease);
    // Caméra en cours de test (verrou pris) : sautée aussi.
    let lock = hub.camera_probe_lock(id);
    let guard = lock.lock().unwrap();
    run_monitor(&hub, vec![Wake::Camera(id)], half).await;
    assert_eq!(describes.load(Ordering::SeqCst), 2);
    drop(guard);
    run_monitor(&hub, vec![Wake::Camera(id)], half).await;
    assert_eq!(describes.load(Ordering::SeqCst), 3);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_first_failure_of_a_reachable_camera_is_rechecked_at_once() {
    let (port, describes) = fake_camera(Camera::OpenOnce).await;
    let (hub, id, dir) = hub_with_camera("mon-recheck", port);
    run_monitor(&hub, vec![Wake::Camera(id)], Duration::from_millis(600)).await;
    assert_eq!(hub.cameras()[0].status, CameraStatus::Online);
    // La caméra « disparaît » : un premier échec (rien ne change), revérifié aussitôt par la
    // boucle elle-même ; le second déclare la caméra injoignable. Trois DESCRIBE en tout.
    run_monitor(&hub, vec![Wake::Camera(id)], Duration::from_millis(900)).await;
    assert_eq!(describes.load(Ordering::SeqCst), 3);
    assert!(matches!(
        hub.cameras()[0].status,
        CameraStatus::Unreachable {
            problem: CameraProblem::NotFound,
            ..
        }
    ));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_offline_time_is_set_once_after_the_two_failures_and_cleared_on_recovery() {
    let mut book = CameraBook::default();
    let id = book.add(&draft("x", &format!("rtsp://{HOST}/a"))).unwrap();
    let info = || StreamInfo {
        codec: "h264".into(),
        width: Some(1),
        height: Some(1),
    };
    let fail = |p: CameraProblem| Err((p, "d".to_string()));
    let since = |book: &CameraBook| match book.status(id) {
        Some(CameraStatus::Unreachable { since, .. }) => *since,
        _ => None,
    };
    book.record_probe_at(id, Ok(info()), 100);
    // Premier échec d'une caméra joignable : toujours en ligne, aucune heure.
    book.record_probe_at(id, fail(CameraProblem::Unreachable), 160);
    assert_eq!(since(&book), None);
    // Deuxième échec : l'heure est celle de ce passage.
    book.record_probe_at(id, fail(CameraProblem::Unreachable), 220);
    assert_eq!(since(&book), Some(220));
    // Elle ne bouge plus tant que la caméra reste injoignable, même si le motif change.
    book.record_probe_at(id, fail(CameraProblem::Unreachable), 280);
    assert_eq!(since(&book), Some(220));
    book.record_probe_at(id, fail(CameraProblem::Unauthorized), 340);
    assert_eq!(since(&book), Some(220));
    // Retour en ligne : rien ; une nouvelle panne repart d'une nouvelle heure.
    book.record_probe_at(id, Ok(info()), 400);
    assert_eq!(book.status(id), Some(&CameraStatus::Online));
    book.record_probe_at(id, fail(CameraProblem::Unreachable), 460);
    book.record_probe_at(id, fail(CameraProblem::Unreachable), 520);
    assert_eq!(since(&book), Some(520));
}
