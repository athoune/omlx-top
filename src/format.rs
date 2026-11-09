//! Human-readable formatting helpers.
//!
//! The exact examples in SPECS.md section 11 are covered by unit tests:
//! `999 B`, `1.0K`, `10.6G`, `20h11m`, `15.6k tok/s`.

const BYTE_UNITS: [&str; 6] = ["B", "K", "M", "G", "T", "P"];

/// Format a byte count with binary (1024-based) units.
///
/// Values below 1 KiB keep the ` B` suffix and no decimals; larger values use
/// one decimal and the usual `K`/`M`/`G`/`T`/`P` suffix.
pub fn bytes(value: u64) -> String {
    if value < 1024 {
        return format!("{value} B");
    }

    let mut scaled = value as f64;
    let mut unit = 0usize;
    while scaled >= 1024.0 && unit + 1 < BYTE_UNITS.len() {
        scaled /= 1024.0;
        unit += 1;
    }

    // Rounding can push a value just below the next unit over the boundary
    // (for example 1048575 bytes rounds to 1024.0K). Move up a unit in that
    // case so the output never says "1024.0K".
    if format!("{scaled:.1}").parse::<f64>().unwrap_or(scaled) >= 1024.0
        && unit + 1 < BYTE_UNITS.len()
    {
        scaled /= 1024.0;
        unit += 1;
    }

    format!("{scaled:.1}{}", BYTE_UNITS[unit])
}

/// Format a duration in seconds using the compact forms used by the header.
pub fn duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m{}s", seconds / 60, seconds % 60)
    } else if seconds < 86_400 {
        format!("{}h{}m", seconds / 3600, (seconds % 3600) / 60)
    } else {
        format!("{}d{}h", seconds / 86_400, (seconds % 86_400) / 3600)
    }
}

/// Format an already-scaled percentage (100.0 means 100 %).
pub fn percent(value: f64) -> String {
    if !value.is_finite() {
        return "-".to_string();
    }

    let text = if value.abs() < 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    };

    let text = text.strip_suffix(".0").unwrap_or(&text);
    format!("{text}%")
}

/// Convert a `0.0..=1.0` ratio to a formatted percentage.
pub fn ratio_percent(ratio: f64) -> String {
    percent(ratio * 100.0)
}

/// Format a token throughput value. Uses decimal (1000-based) units, matching
/// the `15.6k tok/s` example in SPECS.md section 11.
pub fn throughput(value: f64) -> String {
    if !value.is_finite() {
        return "- tok/s".to_string();
    }
    if value < 0.0 {
        return format!("{} tok/s", value.round());
    }

    if value >= 1000.0 {
        let units = ["", "k", "M", "G", "T"];
        let mut scaled = value;
        let mut unit = 0usize;
        while scaled >= 1000.0 && unit + 1 < units.len() {
            scaled /= 1000.0;
            unit += 1;
        }

        let mut text = format!("{scaled:.1}");
        if text.parse::<f64>().unwrap_or(scaled) >= 1000.0 && unit + 1 < units.len() {
            scaled /= 1000.0;
            unit += 1;
            text = format!("{scaled:.1}");
        }

        return format!("{text}{} tok/s", units[unit]);
    }

    let text = format!("{value:.1}");
    let text = text.strip_suffix(".0").unwrap_or(&text);
    format!("{text} tok/s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_matches_spec_examples() {
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1024), "1.0K");
        assert_eq!(bytes(11_374_576_480), "10.6G");
    }

    #[test]
    fn bytes_boundaries() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1024 * 1024), "1.0M");
        assert_eq!(bytes(1536), "1.5K");
        assert_eq!(bytes(1024 * 1024 - 1), "1.0M");
        assert_eq!(bytes(1024u64.pow(4)), "1.0T");
        assert_eq!(bytes(1024u64.pow(5)), "1.0P");
    }

    #[test]
    fn duration_matches_spec_examples() {
        assert_eq!(duration(72_660), "20h11m");
    }

    #[test]
    fn duration_boundaries() {
        assert_eq!(duration(0), "0s");
        assert_eq!(duration(59), "59s");
        assert_eq!(duration(60), "1m0s");
        assert_eq!(duration(3599), "59m59s");
        assert_eq!(duration(3600), "1h0m");
        assert_eq!(duration(86_399), "23h59m");
        assert_eq!(duration(86_400), "1d0h");
        assert_eq!(duration(172_800), "2d0h");
    }

    #[test]
    fn percentages() {
        assert_eq!(percent(0.0), "0%");
        assert_eq!(percent(0.94), "0.9%");
        assert_eq!(percent(5.3), "5.3%");
        assert_eq!(percent(17.0), "17%");
        assert_eq!(percent(100.0), "100%");
        assert_eq!(ratio_percent(0.78), "78%");
        assert_eq!(percent(f64::NAN), "-");
    }

    #[test]
    fn throughput_matches_spec_examples() {
        assert_eq!(throughput(15_605.1), "15.6k tok/s");
        assert_eq!(throughput(0.0), "0 tok/s");
    }

    #[test]
    fn throughput_boundaries() {
        assert_eq!(throughput(999.0), "999 tok/s");
        assert_eq!(throughput(1000.0), "1.0k tok/s");
        assert_eq!(throughput(999_999.0), "1.0M tok/s");
        assert_eq!(throughput(1_000_000.0), "1.0M tok/s");
        assert_eq!(throughput(1_500_000_000.0), "1.5G tok/s");
        assert_eq!(throughput(f64::INFINITY), "- tok/s");
    }
}
