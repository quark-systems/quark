//! Memory pressure as the OS reports it, scaled to 0.0 (none) to 1.0
//! (critical).

/// `None` when this OS has no pressure signal or it cannot be read.
pub fn read() -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/pressure/memory")
            .ok()
            .and_then(|s| parse_psi(&s))
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("sysctl")
            .args(["-n", "kern.memorystatus_vm_pressure_level"])
            .output()
            .ok()?;
        String::from_utf8(out.stdout)
            .ok()
            .and_then(|s| macos_level(s.trim().parse().ok()?))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// Linux PSI: the share of the last 10 seconds in which some task stalled
/// on memory (`some avg10`, a percentage).
pub fn parse_psi(text: &str) -> Option<f64> {
    let line = text.lines().find(|l| l.starts_with("some "))?;
    let avg10 = line
        .split_whitespace()
        .find_map(|f| f.strip_prefix("avg10="))?;
    let pct: f64 = avg10.parse().ok()?;
    Some((pct / 100.0).clamp(0.0, 1.0))
}

/// macOS `kern.memorystatus_vm_pressure_level`: 1 normal, 2 warning,
/// 4 critical.
pub fn macos_level(level: u32) -> Option<f64> {
    match level {
        1 => Some(0.0),
        2 => Some(0.5),
        4 => Some(1.0),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn psi_some_avg10() {
        let text = "some avg10=12.50 avg60=3.00 avg300=1.00 total=123\n\
                    full avg10=2.00 avg60=0.00 avg300=0.00 total=45\n";
        assert_eq!(parse_psi(text), Some(0.125));
        assert_eq!(parse_psi("full avg10=1.00"), None);
        assert_eq!(parse_psi("some avg10=abc"), None);
    }

    #[test]
    fn macos_levels() {
        assert_eq!(macos_level(1), Some(0.0));
        assert_eq!(macos_level(2), Some(0.5));
        assert_eq!(macos_level(4), Some(1.0));
        assert_eq!(macos_level(3), None);
    }
}
