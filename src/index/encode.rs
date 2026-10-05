//! `i8pv_b64` vector encoding: one signed byte per component plus a
//! per-vector scale (port of Phase 0's `i8pv` variant, P2-D5).
//!
//! Identical input vectors produce identical bytes. The per-component error is
//! bounded by `s / 2 + ε`.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

pub use super::manifest::ENCODING_I8PV_B64;

/// Largest quantised magnitude (`i8` range without `-128`).
const QUANT_MAX: i32 = 127;

/// A quantised vector as stored in a shard line.
#[derive(Debug, Clone, PartialEq)]
pub struct Quantized {
    /// Base64 (standard alphabet, padded) of `dim` signed bytes.
    pub q: String,
    /// Per-vector scale frozen to six significant digits: `value ≈ q[i] × s`.
    pub s: f64,
}

/// Why an encoded vector could not be decoded.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DecodeError {
    /// `q` is not valid base64.
    #[error("invalid base64")]
    Base64,
    /// `q` decodes to a different number of components than the index dimension.
    #[error("expected {expected} components, found {found}")]
    Length {
        /// Dimension recorded in the manifest.
        expected: usize,
        /// Components found in the vector.
        found: usize,
    },
}

/// Scale `vector` to unit L2 norm in place; the zero vector is left unchanged.
pub fn normalize(vector: &mut [f32]) {
    let norm = vector
        .iter()
        .map(|x| f64::from(*x) * f64::from(*x))
        .sum::<f64>()
        .sqrt();
    if norm > 0.0 {
        let inverse = (1.0 / norm) as f32;
        for component in vector {
            *component *= inverse;
        }
    }
}

/// Quantise `vector`: `scale = max|x| / 127` (`1/127` for the zero vector),
/// `q = round(x / scale)` clamped to ±127.
pub fn encode_i8pv(vector: &[f32]) -> Quantized {
    let max_abs = vector.iter().fold(0.0f32, |acc, x| acc.max(x.abs()));
    let scale = if max_abs > 0.0 {
        max_abs / 127.0
    } else {
        1.0 / 127.0
    };
    let bytes: Vec<u8> = vector
        .iter()
        .map(|x| ((x / scale).round() as i32).clamp(-QUANT_MAX, QUANT_MAX) as i8)
        .map(i8::cast_unsigned)
        .collect();
    // Freeze the scale to six significant digits so the bytes do not depend on
    // the platform's float printing (Phase 0 T9.4).
    let frozen = format!("{scale:.6e}")
        .parse::<f64>()
        .unwrap_or_else(|_| f64::from(scale));
    Quantized {
        q: STANDARD.encode(bytes),
        s: frozen,
    }
}

/// Dequantise a stored vector back to `f32`.
pub fn decode_i8pv(q: &str, s: f64, dim: usize) -> Result<Vec<f32>, DecodeError> {
    let bytes = STANDARD.decode(q).map_err(|_| DecodeError::Base64)?;
    if bytes.len() != dim {
        return Err(DecodeError::Length {
            expected: dim,
            found: bytes.len(),
        });
    }
    let scale = s as f32;
    Ok(bytes
        .into_iter()
        .map(|byte| f32::from(byte.cast_signed()) * scale)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_sample() -> Vec<f32> {
        let mut vector: Vec<f32> = (0..384).map(|i| ((i as f32) * 0.013 - 2.5) * 0.1).collect();
        normalize(&mut vector);
        vector
    }

    #[test]
    fn round_trip_error_is_bounded_by_half_scale() {
        let vector = unit_sample();
        let quantized = encode_i8pv(&vector);
        let back = decode_i8pv(&quantized.q, quantized.s, vector.len()).expect("decode");
        for (a, b) in vector.iter().zip(&back) {
            let error = f64::from((a - b).abs());
            assert!(
                error <= quantized.s / 2.0 + 1e-6,
                "{error} > s/2 for s = {}",
                quantized.s
            );
        }
    }

    #[test]
    fn identical_input_gives_identical_bytes() {
        let vector = unit_sample();
        assert_eq!(encode_i8pv(&vector), encode_i8pv(&vector.clone()));
    }

    #[test]
    fn zero_vector_round_trips_to_zeros() {
        let zeros = vec![0.0f32; 4];
        let quantized = encode_i8pv(&zeros);
        assert_eq!(
            decode_i8pv(&quantized.q, quantized.s, 4).expect("decode"),
            zeros
        );
        let mut unchanged = zeros.clone();
        normalize(&mut unchanged);
        assert_eq!(unchanged, zeros);
    }

    #[test]
    fn decode_rejects_wrong_length_and_bad_base64() {
        let quantized = encode_i8pv(&[0.5, -0.5, 0.25]);
        assert_eq!(
            decode_i8pv(&quantized.q, quantized.s, 4),
            Err(DecodeError::Length {
                expected: 4,
                found: 3
            })
        );
        assert_eq!(
            decode_i8pv("not base64!!", 1.0, 3),
            Err(DecodeError::Base64)
        );
    }

    #[test]
    fn normalize_yields_unit_norm() {
        let mut vector = vec![3.0f32, 4.0, 12.0];
        normalize(&mut vector);
        let norm: f64 = vector
            .iter()
            .map(|x| f64::from(*x).powi(2))
            .sum::<f64>()
            .sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "{norm}");
    }

    #[test]
    fn scale_has_six_significant_digits() {
        let quantized = encode_i8pv(&unit_sample());
        let printed = quantized.s.to_string();
        assert_eq!(printed.parse::<f64>().expect("parse"), quantized.s);
        let reformatted: f64 = format!("{:.6e}", quantized.s).parse().expect("parse");
        assert_eq!(reformatted, quantized.s);
    }

    #[test]
    fn largest_component_maps_to_the_quantisation_bound() {
        let quantized = encode_i8pv(&[1.0, -1.0, 0.0]);
        let bytes = STANDARD.decode(&quantized.q).expect("base64");
        let signed: Vec<i8> = bytes.into_iter().map(u8::cast_signed).collect();
        assert_eq!(signed, vec![127, -127, 0]);
    }
}
