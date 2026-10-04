//! Vector encodings for the shard variants (T9.4).
//!
//! Every encoding must round-trip deterministically: identical input vectors
//! produce identical bytes. int8 error is bounded by `s/2 + ε` (per-vector) and
//! by `1/254` (fixed scale).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

/// The five shard variants (T9.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Variant {
    /// `"v":[…]` — serde_json shortest round-trip floats.
    F32Dec,
    /// `"v":"<base64 f32 LE>"`.
    F32B64,
    /// `"q":"<base64 i8>","s":<per-vector scale>`.
    I8pvB64,
    /// `"q":"<base64 i8>"` — fixed global scale 254.
    I8fxB64,
    /// Metadata only (size baseline).
    Nov,
}

impl Variant {
    pub fn id(self) -> &'static str {
        match self {
            Variant::F32Dec => "f32_dec",
            Variant::F32B64 => "f32_b64",
            Variant::I8pvB64 => "i8pv_b64",
            Variant::I8fxB64 => "i8fx_b64",
            Variant::Nov => "nov",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "f32_dec" => Some(Variant::F32Dec),
            "f32_b64" => Some(Variant::F32B64),
            "i8pv_b64" => Some(Variant::I8pvB64),
            "i8fx_b64" => Some(Variant::I8fxB64),
            "nov" => Some(Variant::Nov),
            _ => None,
        }
    }
}

/// All measured variants, in report order.
pub const VARIANTS: &[Variant] = &[
    Variant::F32Dec,
    Variant::F32B64,
    Variant::I8pvB64,
    Variant::I8fxB64,
    Variant::Nov,
];

/// The vector part of a shard line (`v` / `q` / `s`, T9.4 key order).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VectorFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub v: Option<VectorValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub s: Option<f64>,
}

/// `v` is either a decimal array (f32_dec) or a base64 blob (f32_b64).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum VectorValue {
    Dec(Vec<f32>),
    B64(String),
}

/// Encoding statistics recorded by the index step (T9.5 quantization).
#[derive(Debug, Clone, Copy, Default)]
pub struct EncodeStats {
    /// Fraction of components clamped at the int8 bounds (i8fx only).
    pub clip_rate: f64,
}

/// Encode one unit-ish vector into `variant` fields.
pub fn encode(variant: Variant, vector: &[f32]) -> (VectorFields, EncodeStats) {
    match variant {
        Variant::F32Dec => (
            VectorFields {
                v: Some(VectorValue::Dec(vector.to_vec())),
                ..Default::default()
            },
            EncodeStats::default(),
        ),
        Variant::F32B64 => (
            VectorFields {
                v: Some(VectorValue::B64(encode_f32_b64(vector))),
                ..Default::default()
            },
            EncodeStats::default(),
        ),
        Variant::I8pvB64 => {
            let max = vector.iter().fold(0.0f32, |acc, x| acc.max(x.abs()));
            let scale = if max > 0.0 { max / 127.0 } else { 1.0 / 127.0 };
            let q = vector
                .iter()
                .map(|x| ((x / scale).round() as i32).clamp(-127, 127) as i8)
                .collect::<Vec<_>>();
            // Freeze the scale to 6 significant digits so the bytes do not
            // depend on the platform's f32 printing (T9.4).
            let stable: f64 = format!("{scale:.6e}").parse().unwrap_or(scale as f64);
            (
                VectorFields {
                    v: None,
                    q: Some(encode_i8_b64(&q)),
                    s: Some(stable),
                },
                EncodeStats::default(),
            )
        }
        Variant::I8fxB64 => {
            let mut clipped = 0usize;
            let q: Vec<i8> = vector
                .iter()
                .map(|x| {
                    let scaled = (x * 254.0).round() as i32;
                    if !(-127..=127).contains(&scaled) {
                        clipped += 1;
                    }
                    scaled.clamp(-127, 127) as i8
                })
                .collect();
            (
                VectorFields {
                    v: None,
                    q: Some(encode_i8_b64(&q)),
                    s: None,
                },
                EncodeStats {
                    clip_rate: clipped as f64 / vector.len().max(1) as f64,
                },
            )
        }
        Variant::Nov => (VectorFields::default(), EncodeStats::default()),
    }
}

/// Decode shard fields back to f32 (dequantising int8).
pub fn decode(fields: &VectorFields, dim: usize) -> Option<Vec<f32>> {
    if let Some(VectorValue::Dec(values)) = &fields.v {
        return Some(values.clone());
    }
    if let Some(VectorValue::B64(blob)) = &fields.v {
        return decode_f32_b64(blob, dim);
    }
    let q = decode_i8_b64(fields.q.as_deref()?)?;
    if let Some(s) = fields.s {
        return Some(q.iter().map(|x| f32::from(*x) * s as f32).collect());
    }
    // Fixed global scale (i8fx): v = q / 254.
    Some(q.iter().map(|x| f32::from(*x) / 254.0).collect())
}

fn encode_f32_b64(vector: &[f32]) -> String {
    let bytes: Vec<u8> = vector.iter().flat_map(|x| x.to_le_bytes()).collect();
    STANDARD.encode(bytes)
}

fn decode_f32_b64(blob: &str, dim: usize) -> Option<Vec<f32>> {
    let bytes = STANDARD.decode(blob).ok()?;
    if bytes.len() != dim * 4 {
        return None;
    }
    let (chunks, _) = bytes.as_chunks::<4>();
    Some(chunks.iter().map(|c| f32::from_le_bytes(*c)).collect())
}

fn encode_i8_b64(q: &[i8]) -> String {
    let bytes: Vec<u8> = q.iter().map(|x| *x as u8).collect();
    STANDARD.encode(bytes)
}

fn decode_i8_b64(blob: &str) -> Option<Vec<i8>> {
    let bytes = STANDARD.decode(blob).ok()?;
    Some(bytes.iter().map(|b| *b as i8).collect())
}

/// L2 norm of a vector.
pub fn norm(vector: &[f32]) -> f64 {
    vector
        .iter()
        .map(|x| f64::from(*x) * f64::from(*x))
        .sum::<f64>()
        .sqrt()
}

/// Normalise a vector to unit length in place; returns the original norm.
pub fn normalize(vector: &mut [f32]) -> f64 {
    let n = norm(vector);
    if n > 0.0 {
        let inv = (1.0 / n) as f32;
        for x in vector {
            *x *= inv;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<f32> {
        (0..384).map(|i| ((i as f32) * 0.013 - 2.5) * 0.1).collect()
    }

    fn unit_sample() -> Vec<f32> {
        let mut v = sample();
        normalize(&mut v);
        v
    }

    #[test]
    fn f32_variants_round_trip_exactly() {
        let v = unit_sample();
        for variant in [Variant::F32Dec, Variant::F32B64] {
            let (fields, _) = encode(variant, &v);
            let back = decode(&fields, v.len()).expect("decode");
            assert_eq!(back, v, "variant {}", variant.id());
            // Deterministic bytes for identical input.
            let (again, _) = encode(variant, &v);
            assert_eq!(
                serde_json::to_string(&fields).unwrap(),
                serde_json::to_string(&again).unwrap(),
                "variant {} must be deterministic",
                variant.id()
            );
        }
    }

    #[test]
    fn int8_per_vector_error_is_bounded() {
        let v = unit_sample();
        let (fields, _) = encode(Variant::I8pvB64, &v);
        let s = fields.s.expect("per-vector scale");
        let back = decode(&fields, v.len()).expect("decode");
        for (a, b) in v.iter().zip(&back) {
            let err = (a - b).abs();
            assert!(f64::from(err) <= s / 2.0 + 1e-6, "err {err} > s/2 {s}");
        }
    }

    #[test]
    fn int8_fixed_scale_error_is_bounded_and_clip_is_zero() {
        let v = unit_sample();
        let (fields, stats) = encode(Variant::I8fxB64, &v);
        assert_eq!(stats.clip_rate, 0.0, "unit vectors stay within the scale");
        let back = decode(&fields, v.len()).expect("decode");
        for (a, b) in v.iter().zip(&back) {
            assert!((a - b).abs() <= 1.0 / 254.0 + 1e-6);
        }
    }

    #[test]
    fn fixed_scale_clamps_out_of_range() {
        let big = vec![2.0f32; 8];
        let (fields, stats) = encode(Variant::I8fxB64, &big);
        assert!(stats.clip_rate > 0.9, "clip rate {}", stats.clip_rate);
        let back = decode(&fields, 8).expect("decode");
        assert!(back.iter().all(|x| (x - 127.0 / 254.0).abs() < 1e-6));
    }

    #[test]
    fn nov_has_no_vector_fields() {
        let (fields, _) = encode(Variant::Nov, &unit_sample());
        assert!(fields.v.is_none() && fields.q.is_none() && fields.s.is_none());
        assert!(decode(&fields, 384).is_none());
    }

    #[test]
    fn zero_vector_encodes_safely() {
        let zeros = vec![0.0f32; 4];
        let (fields, _) = encode(Variant::I8pvB64, &zeros);
        let back = decode(&fields, 4).expect("decode");
        assert_eq!(back, vec![0.0; 4]);
    }
}
