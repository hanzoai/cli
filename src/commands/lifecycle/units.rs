//! Human-readable memory & disk size parsing and formatting for canonical
//! lifecycle commands (`32GiB`, `8GiB`, `8192MiB`, `512MiB`, plain `4096` = MiB).

/// Parse a size specification into MiB (`u64`).
///
/// Accepted forms (case-insensitive):
/// - Plain integer: `"4096"` → `4096` MiB (backward compatible with MB flags)
/// - MiB / MB / M: `"512MiB"` → `512`, `"8192MB"` → `8192`, `"8192M"` → `8192`
/// - GiB / GB / G: `"32GiB"` → `32768`, `"32GB"` → `32768`, `"8G"` → `8192`
/// - TiB / TB / T: `"1TiB"` → `1048576`, `"2TB"` → `2097152`
/// - Fractional GiB / TiB: `"1.5GiB"` → `1536`
pub fn parse_size_mb(input: &str) -> std::result::Result<u64, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("size cannot be empty".into());
    }
    let split = trimmed
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '_')
        .unwrap_or(trimmed.len());
    let num_part = trimmed[..split].replace('_', "");
    let unit_part = trimmed[split..].trim().to_ascii_lowercase();

    if num_part.is_empty() {
        return Err(format!(
            "invalid size `{input}`: expected a positive size such as `8GiB`, `4096MiB`, or `4096`"
        ));
    }

    let mb = match unit_part.as_str() {
        "" | "m" | "mb" | "mib" => parse_scaled(&num_part, 1, input)?,
        "g" | "gb" | "gib" => parse_scaled(&num_part, 1024, input)?,
        "t" | "tb" | "tib" => parse_scaled(&num_part, 1024 * 1024, input)?,
        _ => {
            return Err(format!(
                "unsupported unit in `{input}` (use GiB, GB, G, MiB, MB, M, TiB, or plain MiB integer)"
            ))
        }
    };

    if mb == 0 {
        return Err(format!("size `{input}` must be at least 1 MiB"));
    }
    Ok(mb)
}

fn parse_scaled(num_part: &str, factor: u64, raw: &str) -> std::result::Result<u64, String> {
    if !num_part.contains('.') {
        let n: u64 = num_part
            .parse()
            .map_err(|_| format!("invalid integer size `{raw}`"))?;
        return n
            .checked_mul(factor)
            .ok_or_else(|| format!("size `{raw}` overflows maximum MiB"));
    }
    let val: f64 = num_part
        .parse()
        .map_err(|_| format!("invalid numeric size `{raw}`"))?;
    if !val.is_finite() || val <= 0.0 {
        return Err(format!("invalid size `{raw}`"));
    }
    let scaled = (val * (factor as f64)).round();
    if scaled <= 0.0 || scaled > (u64::MAX as f64) {
        return Err(format!("size `{raw}` out of range"));
    }
    Ok(scaled as u64)
}

/// Format a MiB value cleanly (`32768` → `"32 GiB"`, `1536` → `"1536 MiB"`).
pub fn format_size_mb(mb: u64) -> String {
    if mb >= 1024 * 1024 && mb.is_multiple_of(1024 * 1024) {
        format!("{} TiB", mb / (1024 * 1024))
    } else if mb >= 1024 && mb.is_multiple_of(1024) {
        format!("{} GiB", mb / 1024)
    } else {
        format!("{mb} MiB")
    }
}
