//! Décodeur TLV Matter minimal, utilisé pour lire les réponses de l'Interaction Model.
//!
//! Le décodeur de `matc` lit les entiers signés sur 1 octet comme non signés
//! (−5 devient 251), ce qui fausse les températures négatives proches de 0 °C.
//! On décode donc nous-mêmes les octets bruts (`Message::payload`).

use anyhow::{bail, Context, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Signed(i64),
    Unsigned(u64),
    Bool(bool),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Null,
    /// Structure, tableau ou liste : éléments (tag, valeur). Tag anonyme = `None`.
    Container(Vec<(Option<u32>, Value)>),
}

impl Value {
    /// Premier élément portant le tag de contexte `tag`.
    pub fn field(&self, tag: u32) -> Option<&Value> {
        match self {
            Value::Container(items) => items.iter().find(|(t, _)| *t == Some(tag)).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Suit un chemin de tags de contexte.
    pub fn path(&self, tags: &[u32]) -> Option<&Value> {
        tags.iter().try_fold(self, |v, t| v.field(*t))
    }

    pub fn items(&self) -> &[(Option<u32>, Value)] {
        match self {
            Value::Container(items) => items,
            _ => &[],
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Unsigned(u) => Some(*u),
            Value::Signed(i) if *i >= 0 => Some(*i as u64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).context("débordement TLV")?;
        if end > self.data.len() {
            bail!("TLV tronqué");
        }
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn uint(&mut self, n: usize) -> Result<u64> {
        let b = self.take(n)?;
        Ok(b.iter().rev().fold(0u64, |acc, x| (acc << 8) | *x as u64))
    }

    fn int(&mut self, n: usize) -> Result<i64> {
        let u = self.uint(n)?;
        let shift = 64 - 8 * n as u32;
        Ok(((u << shift) as i64) >> shift)
    }
}

const END_OF_CONTAINER: u8 = 0x18;

/// Décode un élément TLV complet (généralement la structure anonyme d'un message IM).
pub fn decode(data: &[u8]) -> Result<Value> {
    let mut r = Reader { data, pos: 0 };
    match read_element(&mut r)? {
        Some((_, v)) => Ok(v),
        None => bail!("TLV vide"),
    }
}

/// Lit un élément ; retourne `None` sur un marqueur de fin de conteneur.
fn read_element(r: &mut Reader) -> Result<Option<(Option<u32>, Value)>> {
    let control = r.take(1)?[0];
    let elem_type = control & 0x1f;
    if elem_type == END_OF_CONTAINER {
        return Ok(None);
    }
    let tag = match control >> 5 {
        0 => None,
        1 => Some(r.uint(1)? as u32),
        2 | 4 => Some(r.uint(2)? as u32),
        3 | 5 => Some(r.uint(4)? as u32),
        6 => {
            r.take(4)?;
            Some(r.uint(2)? as u32)
        }
        _ => {
            r.take(4)?;
            Some(r.uint(4)? as u32)
        }
    };
    let value = match elem_type {
        0x00..=0x03 => Value::Signed(r.int(1 << elem_type)?),
        0x04..=0x07 => Value::Unsigned(r.uint(1 << (elem_type - 4))?),
        0x08 => Value::Bool(false),
        0x09 => Value::Bool(true),
        0x0A => Value::Float(f32::from_bits(r.uint(4)? as u32) as f64),
        0x0B => Value::Float(f64::from_bits(r.uint(8)?)),
        0x0C..=0x0F => {
            let len = r.uint(1 << (elem_type - 0x0C))? as usize;
            Value::Str(String::from_utf8_lossy(r.take(len)?).into_owned())
        }
        0x10..=0x13 => {
            let len = r.uint(1 << (elem_type - 0x10))? as usize;
            Value::Bytes(r.take(len)?.to_vec())
        }
        0x14 => Value::Null,
        0x15..=0x17 => {
            let mut items = Vec::new();
            while let Some(item) = read_element(r)? {
                items.push(item);
            }
            Value::Container(items)
        }
        other => bail!("type TLV inconnu 0x{other:02x}"),
    };
    Ok(Some((tag, value)))
}

/// Une valeur d'attribut reçue dans un ReportData.
#[derive(Debug, Clone, PartialEq)]
pub struct AttributeReport {
    pub endpoint: u16,
    pub cluster: u32,
    pub attribute: u32,
    /// `Ok(valeur)` ou `Err(code de statut IM)` (ex. 0xC3 = UNSUPPORTED_CLUSTER).
    pub data: std::result::Result<Value, u64>,
}

/// Extrait les rapports d'attributs d'un message ReportData (payload brut).
pub fn parse_report_data(payload: &[u8]) -> Result<Vec<AttributeReport>> {
    let root = decode(payload)?;
    let mut out = Vec::new();
    let Some(reports) = root.field(1) else {
        return Ok(out);
    };
    for (_, report) in reports.items() {
        if let Some(data_ib) = report.field(1) {
            let (Some(ep), Some(cl), Some(at)) = (
                data_ib.path(&[1, 2]).and_then(Value::as_u64),
                data_ib.path(&[1, 3]).and_then(Value::as_u64),
                data_ib.path(&[1, 4]).and_then(Value::as_u64),
            ) else {
                continue;
            };
            let Some(data) = data_ib.field(2) else {
                continue;
            };
            out.push(AttributeReport {
                endpoint: ep as u16,
                cluster: cl as u32,
                attribute: at as u32,
                data: Ok(data.clone()),
            });
        } else if let Some(status_ib) = report.field(0) {
            let ep = status_ib.path(&[0, 2]).and_then(Value::as_u64).unwrap_or(0);
            let cl = status_ib.path(&[0, 3]).and_then(Value::as_u64).unwrap_or(0);
            let at = status_ib.path(&[0, 4]).and_then(Value::as_u64).unwrap_or(0);
            let status = status_ib.path(&[1, 0]).and_then(Value::as_u64).unwrap_or(1);
            out.push(AttributeReport {
                endpoint: ep as u16,
                cluster: cl as u32,
                attribute: at as u32,
                data: Err(status),
            });
        }
    }
    Ok(out)
}

/// MaxInterval (secondes) négocié, lu dans un SubscribeResponse.
pub fn parse_subscribe_response_max_interval(payload: &[u8]) -> Option<u64> {
    decode(payload).ok()?.field(2)?.as_u64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_ints_are_sign_extended() {
        // int8 tag ctx 2 = -5
        assert_eq!(decode(&[0x20, 0x02, 0xFB]).unwrap(), Value::Signed(-5));
        // int16 anonyme = 2150
        assert_eq!(decode(&[0x01, 0x66, 0x08]).unwrap(), Value::Signed(2150));
        // int16 = -32768 (null de MeasuredValue)
        assert_eq!(decode(&[0x01, 0x00, 0x80]).unwrap(), Value::Signed(-32768));
        // uint16 = 4550
        assert_eq!(decode(&[0x05, 0xC6, 0x11]).unwrap(), Value::Unsigned(4550));
    }

    #[test]
    fn parses_report_data() {
        // ReportData { 1: [ AttributeReportIB { 1: AttributeDataIB {
        //   0: DataVersion=1, 1: Path{2: ep 1, 3: 0x0402, 4: 0}, 2: int8 -5 } } ], 0xFF: 11 }
        let payload = [
            0x15, // struct anonyme
            0x36, 0x01, // tableau, tag 1
            0x15, // AttributeReportIB
            0x35, 0x01, // AttributeDataIB, tag 1
            0x24, 0x00, 0x01, // DataVersion
            0x37, 0x01, // liste Path, tag 1
            0x24, 0x02, 0x01, // endpoint 1
            0x25, 0x03, 0x02, 0x04, // cluster 0x0402
            0x24, 0x04, 0x00, // attribut 0
            0x18, // fin Path
            0x20, 0x02, 0xFB, // Data = int8 -5
            0x18, 0x18, // fin AttributeDataIB, AttributeReportIB
            0x18, // fin tableau
            0x24, 0xFF, 0x0B, // IM revision
            0x18,
        ];
        let reports = parse_report_data(&payload).unwrap();
        assert_eq!(reports.len(), 1);
        let r = &reports[0];
        assert_eq!((r.endpoint, r.cluster, r.attribute), (1, 0x0402, 0));
        assert_eq!(r.data, Ok(Value::Signed(-5)));
    }

    #[test]
    fn parses_attribute_status() {
        // ReportData { 1: [ { 0: AttributeStatusIB { 0: Path{2:1,3:0x0405,4:0}, 1: Status{0: 0xC3} } } ] }
        let payload = [
            0x15, 0x36, 0x01, 0x15, 0x35, 0x00, 0x37, 0x00, 0x24, 0x02, 0x01, 0x25, 0x03, 0x05,
            0x04, 0x24, 0x04, 0x00, 0x18, 0x35, 0x01, 0x24, 0x00, 0xC3, 0x18, 0x18, 0x18, 0x18,
            0x18,
        ];
        let reports = parse_report_data(&payload).unwrap();
        assert_eq!(reports[0].cluster, 0x0405);
        assert_eq!(reports[0].data, Err(0xC3));
    }

    #[test]
    fn parses_subscribe_response() {
        // { 0: SubscriptionId=0x1234 (uint16), 2: MaxInterval=60, 0xFF: 11 }
        let payload = [
            0x15, 0x25, 0x00, 0x34, 0x12, 0x24, 0x02, 0x3C, 0x24, 0xFF, 0x0B, 0x18,
        ];
        assert_eq!(parse_subscribe_response_max_interval(&payload), Some(60));
    }

    #[test]
    fn truncated_input_is_error() {
        assert!(decode(&[0x15, 0x25, 0x00]).is_err());
        assert!(decode(&[]).is_err());
    }
}
