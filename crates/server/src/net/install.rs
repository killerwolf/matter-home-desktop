//! Installation sur l'iPhone depuis l'app web (page « Widgets et raccourcis »), sans rien taper.
//! Tout est **personnel** : chaque personne ne voit, ne crée et ne révoque que ses propres jetons.
//!
//! | route | effet |
//! |---|---|
//! | `GET /api/tokens` | mes jetons (raccourcis et widgets : libellé, date) et l'état de chaque type |
//! | `POST /api/tokens/mode/shortcuts` `{"origin", "label"?}` | mon jeton de mode pour ce téléphone, quatre raccourcis signés à télécharger |
//! | `POST /api/tokens/read/widget` `{"origin", "label"?}` | mon jeton de lecture pour ce téléphone, script du widget prérempli |
//! | `DELETE /api/tokens/{id}` | révoque un de mes jetons (`204`, `404` si ce n'est pas le mien) |
//! | `DELETE /api/tokens/mode`, `DELETE /api/tokens/read` | révoque tous mes jetons de ce type (`204`) |
//! | `GET /app/raccourcis/perso/{id}/{fichier}` | un raccourci préparé, **sans jeton** (lien à toucher) |
//!
//! Toutes les routes `/api/tokens…` exigent une session (jamais un jeton de mode ou de lecture).
//! Le libellé (« iPhone de Hamdi ») est celui de la connexion qui demande, sauf si la page en
//! envoie un ; il ne sert qu'à l'affichage. Le jeton est rattaché à la **connexion** qui le
//! demande : réinstaller depuis la même connexion remplace son jeton, jamais ceux des autres
//! téléphones (même s'ils portent le même nom) ni des autres personnes, et couper la connexion
//! révoque ses raccourcis et son widget. Une préparation à la fois : une autre pendant ce temps
//! répond `409 busy` tout de suite, sans faire la queue.
//!
//! Les raccourcis portent le jeton de mode : un lien de Safari ne peut pas poser d'en-tête
//! `Authorization`, ils sont donc servis par une adresse **imprévisible** (128 bits), gardée en
//! mémoire seulement (dans le `Hub`), valable 5 minutes, et remplacée par la préparation suivante
//! **de la même personne**. Elle n'est pas à usage unique : l'app installée sur l'écran d'accueil
//! peut confier le téléchargement à Safari, qui redemande le fichier ; un lien mort à ce
//! moment-là obligerait à tout recommencer (et à changer encore de jeton). Le jeton ne sert qu'à
//! changer le mode et à dire où l'on est, et l'adresse expire vite.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::Extension;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use protocol::wire::{ApiError, ErrorCode};

use super::routes::{ApiErr, ApiJson};
use super::AppState;
use crate::security::network::PublicAddress;
use crate::security::sessions::SessionInfo;
use crate::security::user_tokens::{clean_label, Kind, TokenInfo};
use crate::shortcuts::{self, HomeShortcut, LINK_TTL};

/// Préfixe des liens (le service worker n'intercepte rien sous `/app/raccourcis/`).
pub const LINK_PREFIX: &str = "/app/raccourcis/perso/";

/// Routes sous session (à fusionner dans le routeur protégé).
pub fn protected() -> Router<AppState> {
    Router::new()
        .route("/api/tokens", get(status))
        .route("/api/tokens/mode", delete(revoke_mode))
        .route("/api/tokens/read", delete(revoke_read))
        .route("/api/tokens/{id}", delete(revoke_one))
        .route("/api/tokens/mode/shortcuts", post(mode_shortcuts))
        .route("/api/tokens/read/widget", post(widget))
}

/// Route publique des téléchargements.
pub fn public() -> Router<AppState> {
    Router::new().route("/app/raccourcis/perso/{id}/{file}", get(download))
}

#[derive(Serialize)]
struct TokenState {
    exists: bool,
    /// Secondes Unix : le plus récent de mes jetons de ce type.
    created: Option<f64>,
}

#[derive(Serialize)]
struct TokenView {
    id: String,
    /// `mode` (raccourcis) ou `read` (widget).
    kind: &'static str,
    label: String,
    /// Secondes Unix.
    created: f64,
}

#[derive(Serialize)]
struct TokensResponse {
    mode: TokenState,
    read: TokenState,
    /// Tous mes jetons, le plus récent d'abord.
    items: Vec<TokenView>,
    /// Le libellé que prendra un nouveau jeton demandé depuis cette connexion.
    label: String,
    /// L'adresse publique réglée sur le serveur (`https://nom`), que les raccourcis et le widget de
    /// l'iPhone savent joindre (certificat officiel) en plus des noms en .ts.net. Absente si rien
    /// n'est réglé.
    #[serde(skip_serializing_if = "Option::is_none")]
    public_origin: Option<String>,
}

fn kind_word(kind: Kind) -> &'static str {
    match kind {
        Kind::Mode => "mode",
        Kind::Read => "read",
    }
}

fn state_of(mine: &[TokenInfo], kind: Kind) -> TokenState {
    let created = mine
        .iter()
        .filter(|t| t.kind == kind)
        .map(|t| t.created)
        .reduce(f64::max);
    TokenState {
        exists: created.is_some(),
        created,
    }
}

/// La connexion (installation) d'où la session demandeuse a ouvert son compte, si elle l'a
/// annoncée : c'est à elle que le jeton est rattaché, pour que la couper le révoque. `None` : un
/// ancien client, qui n'a rien dit de lui.
fn installation_of(state: &AppState, session: &SessionInfo) -> Option<String> {
    state
        .auth
        .sessions
        .connections()
        .into_iter()
        .find(|c| c.session_id == session.id && !c.unknown)
        .map(|c| c.id)
}

/// Le libellé d'un nouveau jeton : celui que la page envoie, sinon le nom de la connexion qui
/// demande (« iPhone de Hamdi »).
fn label_for(state: &AppState, session: &SessionInfo, asked: Option<&str>) -> String {
    let own = state
        .auth
        .sessions
        .connections()
        .into_iter()
        .find(|c| c.session_id == session.id)
        .map(|c| c.name)
        .unwrap_or_default();
    clean_label(
        asked.filter(|a| !a.trim().is_empty()).unwrap_or(&own),
        "Mon téléphone",
    )
}

async fn status(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
) -> Json<TokensResponse> {
    let mine = state.hub.tokens_of(&session.user_id);
    Json(TokensResponse {
        mode: state_of(&mine, Kind::Mode),
        read: state_of(&mine, Kind::Read),
        items: mine
            .iter()
            .map(|t| TokenView {
                id: t.id.clone(),
                kind: kind_word(t.kind),
                label: t.label.clone(),
                created: t.created,
            })
            .collect(),
        label: label_for(&state, &session, None),
        public_origin: state
            .auth
            .network
            .current()
            .public
            .as_ref()
            .map(|p| p.origin().to_string()),
    })
}

async fn revoke_kind(
    state: AppState,
    session: SessionInfo,
    kind: Kind,
) -> Result<StatusCode, ApiErr> {
    let hub = state.hub.clone();
    let user_id = session.user_id.clone();
    let n = blocking(move || hub.revoke_tokens_of(&user_id, kind)).await?;
    log::info!(
        "{n} jeton(s) de {} révoqué(s) depuis l'app web par {}",
        kind.word(),
        state
            .auth
            .accounts
            .display_name(&session.user_id)
            .unwrap_or_default()
    );
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_mode(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
) -> Result<StatusCode, ApiErr> {
    revoke_kind(state, session, Kind::Mode).await
}

async fn revoke_read(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
) -> Result<StatusCode, ApiErr> {
    revoke_kind(state, session, Kind::Read).await
}

async fn revoke_one(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiErr> {
    let hub = state.hub.clone();
    let user_id = session.user_id.clone();
    blocking(move || hub.revoke_token(&user_id, &id)).await?;
    log::info!(
        "un jeton révoqué depuis l'app web par {}",
        state
            .auth
            .accounts
            .display_name(&session.user_id)
            .unwrap_or_default()
    );
    Ok(StatusCode::NO_CONTENT)
}

/// L'adresse de la page, `location.origin` : celle que l'iPhone sait joindre.
#[derive(Deserialize)]
struct OriginRequest {
    origin: String,
    /// Le nom du téléphone (facultatif : sinon celui de la connexion qui demande).
    #[serde(default)]
    label: Option<String>,
}

/// L'adresse de la page, si c'est une adresse que l'iPhone sait joindre : l'adresse publique réglée
/// sur le serveur (même nom exactement), ou un nom en .ts.net (`shortcuts::public_address`).
fn page_address(origin: &str, public: Option<&PublicAddress>) -> Result<String, ApiError> {
    if let (Some(public), Ok(asked)) = (public, PublicAddress::parse(origin)) {
        if &asked == public {
            return Ok(public.origin().to_string());
        }
    }
    shortcuts::public_address(origin)
}

/// L'adresse publique à écrire dans les fichiers : celle de la page (`origin`), l'adresse publique
/// réglée ou un nom en .ts.net (`page_address`), et **celle de ce Mac** :
/// - le navigateur envoie `Origin` avec un `POST` : elle doit être la même que `origin` ;
/// - `tailscale serve` et le relais Caddy transmettent l'en-tête `Host` d'origine (le nom en
///   .ts.net, ou l'adresse publique), comme le prouve le contrôle `Origin`/`Host` des WebSocket, qui
///   passe derrière eux. `X-Forwarded-Host` est lu aussi, au cas où. Si l'un des deux est un nom en
///   .ts.net ou l'adresse publique, l'adresse doit être ce nom-là ; sinon (relais qui réécrit
///   `Host`, par exemple en `localhost:8443`), on garde l'adresse validée.
fn this_mac(
    headers: &HeaderMap,
    origin: &str,
    public: Option<&PublicAddress>,
) -> Result<String, ApiError> {
    let address = page_address(origin, public)?;
    let refuse = || {
        ApiError::with_detail(
            ErrorCode::BadRequest,
            "Cette adresse n'est pas celle de ce Mac : ouvre l'app par son adresse en .ts.net ou par l'adresse publique réglée sur le serveur.",
        )
    };
    if let Some(sent) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        if page_address(sent, public).ok().as_deref() != Some(address.as_str()) {
            return Err(refuse());
        }
    }
    let host = address.trim_start_matches("https://");
    let names: Vec<String> = [headers.get(header::HOST), headers.get("x-forwarded-host")]
        .into_iter()
        .flatten()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(',').next())
        .map(|v| v.trim().to_ascii_lowercase())
        .map(|v| v.strip_suffix(":443").map(str::to_string).unwrap_or(v))
        .filter(|v| v.ends_with(".ts.net") || public.is_some_and(|p| p.matches_host(v)))
        .collect();
    if !names.is_empty() && !names.iter().any(|name| name == host) {
        return Err(refuse());
    }
    Ok(address)
}

/// Tant que le gestionnaire de la requête vit, la préparation est voulue. Si la requête est
/// abandonnée (client parti, délai de 120 s), axum lâche le gestionnaire : le drapeau tombe, et la
/// préparation, qui continue dans son thread, ne remplace pas le jeton (personne n'aurait les
/// fichiers).
struct StillWanted(Arc<AtomicBool>);

impl StillWanted {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }

    fn probe(&self) -> impl Fn() -> bool + Send + 'static {
        let flag = self.0.clone();
        move || flag.load(Ordering::SeqCst)
    }

    /// La sonde passée au hub, vérifiée sous le verrou `Change`, juste avant d'installer : la requête
    /// attend toujours **et** la session qui la demande vit encore. Une coupure de la connexion
    /// (`DELETE /api/me/installations/{id}`) qui se termine entre l'authentification et
    /// l'installation ne laisse donc pas un jeton neuf pour une connexion déjà coupée.
    fn probe_live(
        &self,
        state: &AppState,
        session: &SessionInfo,
    ) -> impl Fn() -> bool + Send + 'static {
        let wanted = self.probe();
        let (auth, session) = (state.auth.clone(), session.clone());
        move || wanted() && auth.is_live(&session)
    }
}

impl Drop for StillWanted {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[derive(Serialize)]
struct ShortcutLink {
    /// Nom du raccourci, tel que le widget l'appelle.
    name: &'static str,
    /// Lien relatif à l'origine, à toucher.
    url: String,
}

#[derive(Serialize)]
struct ShortcutsResponse {
    shortcuts: Vec<ShortcutLink>,
    expires_in_s: u64,
}

/// Mon jeton de mode pour ce téléphone et les quatre raccourcis qui le portent, signés. Le jeton ne
/// remplace celui du même téléphone qu'une fois les quatre signatures réussies, et si la requête
/// attend toujours : un échec ou un abandon ne casse rien de ce qui marche. Les raccourcis des
/// autres personnes (et mes autres téléphones) ne sont jamais touchés.
async fn mode_shortcuts(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<OriginRequest>,
) -> Result<Json<ShortcutsResponse>, ApiErr> {
    let address = this_mac(
        &headers,
        &request.origin,
        state.auth.network.current().public.as_ref(),
    )?;
    let (hub, signer) = (state.hub.clone(), state.signer);
    let label = label_for(&state, &session, request.label.as_deref());
    let installation = installation_of(&state, &session);
    let user_id = session.user_id.clone();
    let wanted = StillWanted::new();
    let still_wanted = wanted.probe_live(&state, &session);
    let ids = blocking(move || {
        hub.prepare_mode_shortcuts(
            &user_id,
            &label,
            installation.as_deref(),
            |token| signer.sign_shortcuts(&address, token),
            still_wanted,
        )
    })
    .await?;
    drop(wanted);
    log::info!(
        "nouveau jeton de mode et raccourcis préparés depuis l'app web pour {}",
        state
            .auth
            .accounts
            .display_name(&session.user_id)
            .unwrap_or_default()
    );
    let shortcuts = HomeShortcut::ALL
        .iter()
        .zip(ids)
        .map(|(which, id)| ShortcutLink {
            name: which.name(),
            url: format!(
                "{LINK_PREFIX}{id}/{}",
                which.file_name().replace(' ', "%20")
            ),
        })
        .collect();
    Ok(Json(ShortcutsResponse {
        shortcuts,
        expires_in_s: LINK_TTL.as_secs(),
    }))
}

#[derive(Serialize)]
struct WidgetResponse {
    script: String,
}

/// Mon jeton de lecture pour ce téléphone et le script du widget qui le porte (à copier dans
/// Scriptable).
async fn widget(
    State(state): State<AppState>,
    Extension(session): Extension<SessionInfo>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<OriginRequest>,
) -> Result<Json<WidgetResponse>, ApiErr> {
    let address = this_mac(
        &headers,
        &request.origin,
        state.auth.network.current().public.as_ref(),
    )?;
    let hub = state.hub.clone();
    let label = label_for(&state, &session, request.label.as_deref());
    let installation = installation_of(&state, &session);
    let user_id = session.user_id.clone();
    let wanted = StillWanted::new();
    let still_wanted = wanted.probe_live(&state, &session);
    let script = blocking(move || {
        hub.prepare_widget(
            &user_id,
            &label,
            installation.as_deref(),
            |token| Ok(shortcuts::widget_script(&address, token)),
            still_wanted,
        )
    })
    .await?;
    drop(wanted);
    log::info!(
        "nouveau jeton de lecture et script du widget préparés depuis l'app web pour {}",
        state
            .auth
            .accounts
            .display_name(&session.user_id)
            .unwrap_or_default()
    );
    Ok(Json(WidgetResponse { script }))
}

async fn download(
    State(state): State<AppState>,
    method: Method,
    Path((id, file)): Path<(String, String)>,
) -> Response {
    let Some(body) = state.hub.shortcut_download(&id, &file) else {
        return ApiErr(ApiError::with_detail(
            ErrorCode::NotFound,
            "lien expiré : recommencez depuis l'app web",
        ))
        .into_response();
    };
    let mut response = if method == Method::HEAD {
        StatusCode::OK.into_response()
    } else {
        (StatusCode::OK, body).into_response()
    };
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    // Le nom est l'un des noms de `HomeShortcut::ALL`, en ASCII (voir `shortcuts`).
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{file}\"")) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    super::webapp::security_headers(headers);
    response
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiErr> {
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result.map_err(ApiErr),
        Err(_) => Err(ApiErr(ApiError::new(ErrorCode::Internal))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, value.parse().unwrap());
        }
        map
    }

    #[test]
    fn the_address_must_be_this_macs_when_the_request_says_so() {
        let ok = "https://maison.tailnet-1.ts.net";
        // `tailscale serve` : Host est le nom en .ts.net, Origin la page.
        let h = headers(&[("host", "maison.tailnet-1.ts.net"), ("origin", ok)]);
        assert_eq!(this_mac(&h, ok, None).unwrap(), ok);
        assert_eq!(
            this_mac(
                &headers(&[("host", "Maison.tailnet-1.ts.net:443")]),
                ok,
                None
            )
            .unwrap(),
            ok
        );
        // Un autre Mac du tailnet, par Host ou par X-Forwarded-Host : refusé.
        for h in [
            headers(&[("host", "autre.tailnet-1.ts.net")]),
            headers(&[
                ("host", "localhost:8443"),
                ("x-forwarded-host", "autre.tailnet-1.ts.net"),
            ]),
            headers(&[
                ("host", "maison.tailnet-1.ts.net"),
                ("origin", "https://autre.tailnet-1.ts.net"),
            ]),
        ] {
            let e = this_mac(&h, ok, None).unwrap_err();
            assert!(e.detail.unwrap().contains("ce Mac"));
        }
        // Un relais qui réécrit Host (pas de nom en .ts.net) : l'adresse validée est gardée.
        assert_eq!(
            this_mac(&headers(&[("host", "localhost:8443")]), ok, None).unwrap(),
            ok
        );
        // L'adresse elle-même reste contrôlée.
        assert!(this_mac(
            &headers(&[("host", "192.168.1.20:8443")]),
            "https://192.168.1.20:8443",
            None
        )
        .is_err());
    }

    #[test]
    fn the_configured_public_address_is_accepted_and_nothing_else_looks_like_it() {
        let public = PublicAddress::parse("https://ostal.klbr.h4md1.fr").unwrap();
        let p = Some(&public);
        let ok = "https://ostal.klbr.h4md1.fr";
        // Caddy garde Host et la page envoie Origin.
        let h = headers(&[("host", "ostal.klbr.h4md1.fr"), ("origin", ok)]);
        assert_eq!(this_mac(&h, ok, p).unwrap(), ok);
        // Host réécrit par un relais : l'adresse validée est gardée.
        assert_eq!(
            this_mac(&headers(&[("host", "localhost:8443")]), ok, p).unwrap(),
            ok
        );
        // Sans adresse publique réglée, ce nom est refusé.
        assert!(this_mac(&h, ok, None).is_err());
        // Un autre nom ressemblant, ou un port, n'est pas l'adresse réglée.
        for bad in [
            "https://evilostal.klbr.h4md1.fr",
            "https://x.ostal.klbr.h4md1.fr",
            "https://ostal.klbr.h4md1.fr:8443",
            "https://autre.klbr.h4md1.fr",
        ] {
            assert!(this_mac(&headers(&[]), bad, p).is_err(), "{bad}");
        }
        // Origin d'une autre adresse, Host d'un autre nom : refusé.
        assert!(this_mac(
            &headers(&[
                ("host", "ostal.klbr.h4md1.fr"),
                ("origin", "https://m.tailnet-1.ts.net")
            ]),
            ok,
            p
        )
        .is_err());
        assert!(this_mac(&headers(&[("host", "m.tailnet-1.ts.net")]), ok, p).is_err());
        // Et l'adresse en .ts.net marche toujours à côté, mais pas si Host est l'adresse publique.
        let ts = "https://m.tailnet-1.ts.net";
        assert_eq!(
            this_mac(&headers(&[("host", "m.tailnet-1.ts.net")]), ts, p).unwrap(),
            ts
        );
        assert!(this_mac(&headers(&[("host", "ostal.klbr.h4md1.fr")]), ts, p).is_err());
    }

    #[test]
    fn a_dropped_handler_means_the_preparation_is_no_longer_wanted() {
        let wanted = StillWanted::new();
        let probe = wanted.probe();
        assert!(probe());
        drop(wanted);
        assert!(!probe());
    }
}
