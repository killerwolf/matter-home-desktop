//! Analyse des codes d'appairage Matter : code manuel (11 ou 21 chiffres) ou QR `MT:...`.

use anyhow::{bail, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupInfo {
    pub passcode: u32,
    /// Discriminateur. Pour un code manuel, seuls les 4 bits de poids fort sont
    /// connus (`is_short = true`) et la valeur est déjà décalée (bits 8..11).
    pub discriminator: u16,
    pub is_short: bool,
}

impl SetupInfo {
    /// Le discriminateur annoncé en mDNS (clé `D`, 12 bits) correspond-il à ce code ?
    pub fn matches(&self, advertised: u16) -> bool {
        if self.is_short {
            advertised & 0xF00 == self.discriminator
        } else {
            advertised == self.discriminator
        }
    }
}

/// Analyse un code saisi par l'utilisateur.
pub fn parse(input: &str) -> Result<SetupInfo> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        bail!("Le code est vide.");
    }
    if trimmed.len() > 3 && trimmed[..3].eq_ignore_ascii_case("MT:") {
        let info = matc::onboarding::decode_qr_payload(&format!("MT:{}", &trimmed[3..]))
            .map_err(|e| anyhow::anyhow!("Code QR illisible : {e}"))?;
        return Ok(SetupInfo {
            passcode: info.passcode,
            discriminator: info.discriminator,
            is_short: false,
        });
    }
    parse_manual(trimmed)
}

/// Code manuel : les espaces et tirets sont ignorés.
pub fn parse_manual(input: &str) -> Result<SetupInfo> {
    let digits: String = input.chars().filter(|c| !matches!(c, ' ' | '-')).collect();
    if !digits.chars().all(|c| c.is_ascii_digit()) {
        bail!(
            "Le code ne doit contenir que des chiffres (ou commencer par « MT: » pour un code QR)."
        );
    }
    if digits.len() != 11 && digits.len() != 21 {
        bail!(
            "Un code manuel contient 11 chiffres (ou 21) ; {} reçu(s).",
            digits.len()
        );
    }
    let (body, check) = digits.split_at(digits.len() - 1);
    let check = check.as_bytes()[0] - b'0';
    if verhoeff_check_digit(body) != check {
        bail!("Code invalide : le chiffre de contrôle ne correspond pas (faute de frappe ?).");
    }
    let d = |range: std::ops::Range<usize>| body[range].parse::<u32>().unwrap_or(0);
    let first = d(0..1);
    let second = d(1..6);
    let third = d(6..10);
    let has_vid_pid = first & 0x4 != 0;
    if has_vid_pid != (digits.len() == 21) {
        bail!("Code invalide : longueur incohérente avec son premier chiffre.");
    }
    let passcode = (second & 0x3FFF) | (third << 14);
    let discriminator = (((first & 0x3) << 10) | ((second >> 6) & 0x300)) as u16;
    if passcode == 0 || passcode >= 99_999_999 {
        bail!("Code invalide : code secret hors limites.");
    }
    Ok(SetupInfo {
        passcode,
        discriminator,
        is_short: true,
    })
}

const D: [[u8; 10]; 10] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
    [1, 2, 3, 4, 0, 6, 7, 8, 9, 5],
    [2, 3, 4, 0, 1, 7, 8, 9, 5, 6],
    [3, 4, 0, 1, 2, 8, 9, 5, 6, 7],
    [4, 0, 1, 2, 3, 9, 5, 6, 7, 8],
    [5, 9, 8, 7, 6, 0, 4, 3, 2, 1],
    [6, 5, 9, 8, 7, 1, 0, 4, 3, 2],
    [7, 6, 5, 9, 8, 2, 1, 0, 4, 3],
    [8, 7, 6, 5, 9, 3, 2, 1, 0, 4],
    [9, 8, 7, 6, 5, 4, 3, 2, 1, 0],
];
const P: [[u8; 10]; 8] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
    [1, 5, 7, 6, 2, 8, 3, 0, 9, 4],
    [5, 8, 0, 3, 7, 9, 6, 1, 4, 2],
    [8, 9, 1, 6, 0, 4, 3, 5, 2, 7],
    [9, 4, 5, 3, 1, 2, 6, 8, 7, 0],
    [4, 2, 8, 6, 5, 7, 3, 9, 0, 1],
    [2, 7, 9, 3, 8, 0, 6, 4, 1, 5],
    [7, 0, 4, 6, 9, 1, 3, 2, 5, 8],
];
const INV: [u8; 10] = [0, 4, 3, 2, 1, 5, 6, 7, 8, 9];

fn verhoeff_check_digit(digits: &str) -> u8 {
    let mut c = 0usize;
    for (i, ch) in digits.bytes().rev().enumerate() {
        let digit = (ch - b'0') as usize;
        c = D[c][P[(i + 1) % 8][digit] as usize] as usize;
    }
    INV[c]
}

#[cfg(test)]
mod tests {
    use super::*;

    // Code de la spécification / du SDK CHIP : passcode 20202021, discriminateur 3840 (0xF00).
    const SPEC_MANUAL: &str = "34970112332";
    const SPEC_QR: &str = "MT:Y.K9042C00KA0648G00";

    #[test]
    fn parses_spec_manual_code() {
        let info = parse(SPEC_MANUAL).unwrap();
        assert_eq!(info.passcode, 20202021);
        assert_eq!(info.discriminator, 0xF00);
        assert!(info.is_short);
        assert!(info.matches(3840));
        assert!(info.matches(0xF3A));
        assert!(!info.matches(0x0F00 - 0x100));
    }

    #[test]
    fn accepts_dashes_and_spaces() {
        assert_eq!(parse("3497-011-2332").unwrap().passcode, 20202021);
        assert_eq!(parse(" 3497 0112 332 ").unwrap().passcode, 20202021);
    }

    #[test]
    fn rejects_bad_check_digit_and_length() {
        assert!(parse("34970112333").is_err());
        assert!(parse("3497011233").is_err());
        assert!(parse("abc").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn parses_spec_qr_code() {
        let info = parse(SPEC_QR).unwrap();
        assert_eq!(info.passcode, 20202021);
        assert_eq!(info.discriminator, 3840);
        assert!(!info.is_short);
        assert!(info.matches(3840));
        assert!(!info.matches(3841));
    }
}
