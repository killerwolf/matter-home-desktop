//! La cryptographie du web push, avec `ring` seulement :
//!
//! - **chiffrement du message** (RFC 8291, codage `aes128gcm` du RFC 8188) : une clé P-256
//!   éphémère par message, ECDH avec la clé publique de l'appareil (`p256dh`), HKDF-SHA-256 avec
//!   son secret (`auth`) et un sel aléatoire, AES-128-GCM sur un seul enregistrement ;
//! - **VAPID** (RFC 8292) : un jeton JWT ES256 signé par la clé du serveur, qui prouve au service
//!   de push (Apple…) que c'est bien le serveur auquel l'appareil s'est abonné.

use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine as _;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{EcdsaKeyPair, KeyPair};
use ring::{aead, agreement, hkdf};

/// Taille d'un point P-256 non compressé (`0x04 ‖ x ‖ y`).
pub const PUBLIC_KEY_LEN: usize = 65;
/// Taille du secret d'authentification de l'appareil.
pub const AUTH_LEN: usize = 16;
/// Taille d'enregistrement annoncée (un seul enregistrement : le message tient dedans).
const RECORD_SIZE: u32 = 4096;
/// Le plus long message en clair accepté : Apple refuse au-delà de 4 096 octets chiffrés ; on
/// garde de la marge pour l'en-tête (86 octets), le délimiteur et l'étiquette GCM (17).
pub const MAX_PLAINTEXT: usize = 3000;

/// Décode du base64url, avec ou sans `=` (les navigateurs ne s'accordent pas).
pub fn b64url_decode(text: &str) -> Option<Vec<u8>> {
    let trimmed = text.trim();
    if trimmed.contains('=') {
        URL_SAFE.decode(trimmed).ok()
    } else {
        URL_SAFE_NO_PAD.decode(trimmed).ok()
    }
}

pub fn b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Une longueur de sortie HKDF.
struct Len(usize);

impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

fn expand(prk: &hkdf::Prk, info: &[&[u8]], len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    prk.expand(info, Len(len))
        .and_then(|okm| okm.fill(&mut out))
        .expect("longueur HKDF valide");
    out
}

/// Les valeurs intermédiaires du RFC 8291 (exposées pour les tests du vecteur de l'annexe A).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derived {
    pub ikm: Vec<u8>,
    pub cek: Vec<u8>,
    pub nonce: Vec<u8>,
}

/// La dérivation des clés (section 3.4 du RFC 8291).
pub fn derive(
    ecdh_secret: &[u8],
    auth: &[u8],
    ua_public: &[u8],
    as_public: &[u8],
    salt: &[u8],
) -> Derived {
    let prk_key = hkdf::Salt::new(hkdf::HKDF_SHA256, auth).extract(ecdh_secret);
    let ikm = expand(&prk_key, &[b"WebPush: info\0", ua_public, as_public], 32);
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(&ikm);
    let cek = expand(&prk, &[b"Content-Encoding: aes128gcm\0"], 16);
    let nonce = expand(&prk, &[b"Content-Encoding: nonce\0"], 12);
    Derived { ikm, cek, nonce }
}

/// Le corps chiffré, à partir du secret ECDH déjà calculé (fonction pure : le vecteur de test
/// fixe la clé éphémère, ce que `ring` ne permet pas).
///
/// Corps : `sel (16) ‖ rs (u32 grand-boutiste) ‖ idlen (65) ‖ clé publique éphémère ‖
/// AES-128-GCM(message ‖ 0x02)`.
pub fn encrypt_with(
    ecdh_secret: &[u8],
    as_public: &[u8],
    ua_public: &[u8],
    auth: &[u8],
    salt: &[u8; 16],
    plaintext: &[u8],
) -> Result<Vec<u8>, String> {
    if as_public.len() != PUBLIC_KEY_LEN || ua_public.len() != PUBLIC_KEY_LEN {
        return Err("clé publique invalide".into());
    }
    if plaintext.len() > MAX_PLAINTEXT {
        return Err("message trop long".into());
    }
    let keys = derive(ecdh_secret, auth, ua_public, as_public, salt);
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_128_GCM, &keys.cek).map_err(|_| "clé AES")?,
    );
    let nonce = aead::Nonce::try_assume_unique_for_key(&keys.nonce).map_err(|_| "nonce")?;
    // Un seul enregistrement, le dernier : délimiteur 0x02, sans bourrage.
    let mut record = Vec::with_capacity(plaintext.len() + 1 + 16);
    record.extend_from_slice(plaintext);
    record.push(0x02);
    key.seal_in_place_append_tag(nonce, aead::Aad::empty(), &mut record)
        .map_err(|_| "chiffrement impossible")?;
    let mut body = Vec::with_capacity(16 + 4 + 1 + PUBLIC_KEY_LEN + record.len());
    body.extend_from_slice(salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(PUBLIC_KEY_LEN as u8);
    body.extend_from_slice(as_public);
    body.extend_from_slice(&record);
    Ok(body)
}

/// Chiffre un message pour un appareil : clé éphémère et sel neufs à chaque appel.
pub fn encrypt(ua_public: &[u8], auth: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    if ua_public.len() != PUBLIC_KEY_LEN || ua_public[0] != 0x04 || auth.len() != AUTH_LEN {
        return Err("clés de l'abonnement invalides".into());
    }
    let rng = SystemRandom::new();
    let mut salt = [0u8; 16];
    rng.fill(&mut salt).map_err(|_| "aléa indisponible")?;
    let private = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng)
        .map_err(|_| "clé éphémère impossible")?;
    let as_public = private
        .compute_public_key()
        .map_err(|_| "clé éphémère impossible")?;
    let as_public = as_public.as_ref().to_vec();
    let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, ua_public);
    let secret = agreement::agree_ephemeral(private, &peer, |s| s.to_vec())
        .map_err(|_| "clé publique de l'abonnement refusée")?;
    encrypt_with(&secret, &as_public, ua_public, auth, &salt, plaintext)
}

// ---------------------------------------------------------------------------
// VAPID
// ---------------------------------------------------------------------------

/// Le `sub` du jeton : une adresse de contact du serveur d'application. Celle du projet, jamais
/// celle de l'utilisateur (le service de push la voit).
pub const VAPID_SUBJECT: &str = "https://github.com/killerwolf/matter-home-desktop";
/// Durée de validité d'un jeton (un neuf à chaque envoi). Le RFC 8292 accepte 24 h au plus.
pub const VAPID_TTL_S: u64 = 3600;

/// La clé VAPID du serveur.
pub struct VapidKey {
    pair: EcdsaKeyPair,
    pkcs8: Vec<u8>,
}

impl VapidKey {
    pub fn generate() -> Result<VapidKey, String> {
        let rng = SystemRandom::new();
        let pkcs8 =
            EcdsaKeyPair::generate_pkcs8(&ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
                .map_err(|_| "clé VAPID impossible")?;
        Self::from_pkcs8(pkcs8.as_ref())
    }

    pub fn from_pkcs8(pkcs8: &[u8]) -> Result<VapidKey, String> {
        let rng = SystemRandom::new();
        let pair = EcdsaKeyPair::from_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            pkcs8,
            &rng,
        )
        .map_err(|_| "clé VAPID illisible")?;
        Ok(VapidKey {
            pair,
            pkcs8: pkcs8.to_vec(),
        })
    }

    pub fn pkcs8(&self) -> &[u8] {
        &self.pkcs8
    }

    /// La clé publique (65 octets, non compressée).
    pub fn public_key(&self) -> &[u8] {
        self.pair.public_key().as_ref()
    }

    /// La clé publique en base64url sans `=` (`applicationServerKey`, paramètre `k`).
    pub fn public_key_b64(&self) -> String {
        b64url(self.public_key())
    }

    /// Le jeton JWT ES256 pour une origine (`https://web.push.apple.com`), valable jusqu'à `exp`.
    pub fn jwt(&self, audience: &str, exp: u64) -> Result<String, String> {
        let header = b64url(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = serde_json::json!({ "aud": audience, "exp": exp, "sub": VAPID_SUBJECT });
        let claims = b64url(claims.to_string().as_bytes());
        let signing_input = format!("{header}.{claims}");
        let signature = self
            .pair
            .sign(&SystemRandom::new(), signing_input.as_bytes())
            .map_err(|_| "signature VAPID impossible")?;
        Ok(format!("{signing_input}.{}", b64url(signature.as_ref())))
    }

    /// L'en-tête `Authorization` d'un envoi (RFC 8292, schéma `vapid`).
    pub fn authorization(&self, audience: &str, now_unix: u64) -> Result<String, String> {
        Ok(format!(
            "vapid t={}, k={}",
            self.jwt(audience, now_unix + VAPID_TTL_S)?,
            self.public_key_b64()
        ))
    }
}
