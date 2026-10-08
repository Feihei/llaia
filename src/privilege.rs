//! Privilege / account probing for the `llaia doctor` T2 self-check (ADR-0033 L2).
//!
//! T2 (deployment-level least privilege) means running the whole llaia process
//! under a dedicated low-privilege account — see docs/guide/security-hardening.md.
//! Doctor can verify that deployment: report the current account and whether the
//! process runs elevated. Zero new dependencies — probes are pure std plus
//! spawning the platform's built-in identity tools (`whoami` on Windows,
//! `id` on Unix), same spirit as the rest of doctor's environment checks.
//!
//! Probes degrade silently to `None` (shown as "unknown") — a failed identity
//! probe is never an error condition.

use std::process::Command;

/// Result of the privilege probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElevationInfo {
    /// Account the process runs as (`whoami` / `id -un`; "<unknown>" on probe failure).
    pub account: String,
    /// `Some(true)` = elevated (Windows High/System integrity, Unix root),
    /// `Some(false)` = not elevated, `None` = probe failed.
    pub elevated: Option<bool>,
}

/// Probe the current account and elevation status. Never panics; a failed
/// probe yields `elevated: None` (account may still carry a best-effort name).
pub fn elevation_info() -> ElevationInfo {
    #[cfg(windows)]
    return windows_probe();
    #[cfg(unix)]
    return unix_probe();
    #[cfg(not(any(windows, unix)))]
    ElevationInfo {
        account: "<unknown>".into(),
        elevated: None,
    }
}

#[cfg(windows)]
fn windows_probe() -> ElevationInfo {
    let account = Command::new("whoami")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "<unknown>".into());

    // Integrity level SIDs are locale-independent, so matching the SID string
    // (not the "High/Medium" label) survives localized Windows output.
    let elevated = Command::new("whoami")
        .args(["/groups", "/fo", "csv"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| parse_windows_group_csv(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or(None);

    ElevationInfo { account, elevated }
}

/// Extract the integrity level from `whoami /groups /fo csv` output.
/// `Some(true)` = High (S-1-16-12288) or System (S-1-16-16384) integrity,
/// `Some(false)` = Medium (S-1-16-8192) or any other integrity level present,
/// `None` = no recognizable integrity row (probe unreliable).
#[cfg(windows)]
fn parse_windows_group_csv(output: &str) -> Option<bool> {
    const HIGH: &str = "S-1-16-12288";
    const SYSTEM: &str = "S-1-16-16384";
    const MEDIUM: &str = "S-1-16-8192";
    let mut found = None;
    for field in output.split(',') {
        let f = field.trim_matches(['"', ' ', '\r', '\n']);
        if f == HIGH || f == SYSTEM {
            return Some(true);
        }
        if f == MEDIUM && found.is_none() {
            found = Some(false);
        }
    }
    found
}

#[cfg(unix)]
fn unix_probe() -> ElevationInfo {
    let account = Command::new("id")
        .args(["-un"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "<unknown>".into());

    let elevated = Command::new("id")
        .args(["-u"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0");

    ElevationInfo { account, elevated }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_csv_detects_medium_integrity() {
        let sample = "\"Group Name\",\"Type\",\"SID\",\"Attributes\"\n\
                      \"Everyone\",\"Well-known group\",\"S-1-1-0\",\"Mandatory group\"\n\
                      \"Mandatory Label\\Medium Mandatory Level\",\"Label\",\"S-1-16-8192\",\"\"\n";
        assert_eq!(parse_windows_group_csv(sample), Some(false));
    }

    #[cfg(windows)]
    #[test]
    fn windows_csv_detects_high_integrity() {
        let sample = "\"Mandatory Label\\High Mandatory Level\",\"Label\",\"S-1-16-12288\",\"\"\n\
                      \"Everyone\",\"Well-known group\",\"S-1-1-0\",\"Mandatory group\"\n";
        assert_eq!(parse_windows_group_csv(sample), Some(true));
    }

    #[cfg(windows)]
    #[test]
    fn windows_csv_detects_system_integrity() {
        let sample =
            "\"Mandatory Label\\System Mandatory Level\",\"Label\",\"S-1-16-16384\",\"\"\n";
        assert_eq!(parse_windows_group_csv(sample), Some(true));
    }

    #[cfg(windows)]
    #[test]
    fn windows_csv_without_integrity_row_is_unknown() {
        let sample = "\"Everyone\",\"Well-known group\",\"S-1-1-0\",\"Mandatory group\"\n";
        assert_eq!(parse_windows_group_csv(sample), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_csv_empty_output_is_unknown() {
        assert_eq!(parse_windows_group_csv(""), None);
    }

    #[cfg(unix)]
    #[test]
    fn unix_probe_reports_current_user() {
        // On CI/dev boxes this is a normal (non-root) account; only assert shape.
        let info = elevation_info();
        assert!(!info.account.is_empty());
        if let Some(e) = info.elevated {
            let root = info.account == "root";
            assert_eq!(e, root, "elevated flag must match the root account");
        }
    }
}
