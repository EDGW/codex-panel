//! Numeric decoding and scaling, independent of extraction formats and billing protocols.
use super::extraction::Scalar;
use crate::Result;

pub fn number(value: Scalar) -> Result<f64> {
    let number = match value {
        Scalar::Number(number) => number,
        Scalar::Text(text) => text.trim().parse().map_err(|_| "invalid numeric string")?,
        _ => return Err("value must be a number or numeric string".into()),
    };
    if !number.is_finite() {
        return Err("value must be finite".into());
    }
    Ok(number)
}

pub fn nonnegative(value: f64) -> Result<f64> {
    if !value.is_finite() || value < 0.0 {
        return Err("value must be finite and nonnegative".into());
    }
    Ok(value)
}
