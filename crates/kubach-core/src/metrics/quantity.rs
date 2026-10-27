//! Kubernetes resource quantities (`250m`, `1.5Gi`, `12345n`, `1e3`).

/// A string that is not a valid Kubernetes quantity.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid quantity `{0}`")]
pub struct QuantityError(pub String);

/// Parses a quantity into its plain value: `"250m"` → `0.25`,
/// `"1Ki"` → `1024.0`. Accepts decimal SI (`n u m k M G T P E`), binary SI
/// (`Ki`…`Ei`) and decimal exponents (`1e3`, `5E-2`).
pub fn parse_quantity(s: &str) -> Result<f64, QuantityError> {
    let err = || QuantityError(s.to_owned());
    let t = s.trim();
    let bytes = t.as_bytes();
    let mut i = 0;
    if matches!(bytes.first(), Some(b'+' | b'-')) {
        i += 1;
    }
    let mut digits = 0;
    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
        digits += 1;
    }
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return Err(err());
    }
    let number: f64 = t[..i].parse().map_err(|_| err())?;
    let multiplier = match &t[i..] {
        "" => 1.0,
        "n" => 1e-9,
        "u" => 1e-6,
        "m" => 1e-3,
        "k" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        "T" => 1e12,
        "P" => 1e15,
        "E" => 1e18,
        "Ki" => 1024f64,
        "Mi" => 1024f64.powi(2),
        "Gi" => 1024f64.powi(3),
        "Ti" => 1024f64.powi(4),
        "Pi" => 1024f64.powi(5),
        "Ei" => 1024f64.powi(6),
        suffix => {
            let exponent = suffix
                .strip_prefix(['e', 'E'])
                .filter(|e| {
                    let unsigned = e.strip_prefix(['+', '-']).unwrap_or(e);
                    !unsigned.is_empty() && unsigned.bytes().all(|b| b.is_ascii_digit())
                })
                .and_then(|e| e.parse::<i32>().ok())
                .ok_or_else(err)?;
            10f64.powi(exponent)
        }
    };
    let value = number * multiplier;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(err())
    }
}

fn non_negative(s: &str, scale: f64) -> Result<u64, QuantityError> {
    let value = parse_quantity(s)?;
    if value < 0.0 {
        return Err(QuantityError(s.to_owned()));
    }
    // `as` saturates for values beyond u64::MAX.
    Ok((value * scale).round() as u64)
}

/// CPU in nanocores: `"250m"` → `250_000_000`, `"2"` → `2_000_000_000`.
pub fn cpu_nanocores(s: &str) -> Result<u64, QuantityError> {
    non_negative(s, 1e9)
}

/// Memory (or storage) in bytes: `"128Mi"` → `134_217_728`.
pub fn memory_bytes(s: &str) -> Result<u64, QuantityError> {
    non_negative(s, 1.0)
}

/// CPU in cores with three decimals: `"0.000"`, `"0.250"`, `"1.500"`. One
/// unit everywhere, so values compare at a glance.
pub fn format_cpu(nanocores: u64) -> String {
    format!("{:.3}", nanocores as f64 / 1e9)
}

/// Memory in MiB with one decimal: `"512.0 MiB"`, `"1536.0 MiB"`. One unit
/// everywhere, so values compare at a glance.
pub fn format_memory(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}
