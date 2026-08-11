//! Unified time source (ADR-0017).
//!
//! Every user-visible timestamp goes through here so that a single
//! `[runtime].timezone` setting controls them all. Without this, `Local::now()`
//! scattered across the codebase silently follows the host TZ — which in a
//! Docker image is UTC, putting memory entries and prompts 8 hours off for a
//! Beijing user.
//!
//! Operational logs (audit.log, sqlite timestamps) intentionally stay on UTC:
//! they are machine-facing and must not shift when the user changes timezone.

use chrono::{Datelike, Local, NaiveDateTime, Timelike, Utc};
use chrono_tz::Tz;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// A wall-clock reading plus the label of the zone it was taken in.
///
/// `naive` deliberately drops the offset: both branches (configured IANA zone
/// and host local time) collapse to the same type, so formatting code does not
/// need to be generic over `TimeZone`.
pub struct Now {
    pub naive: NaiveDateTime,
    pub zone_label: String,
}

impl Now {
    /// `2026-08-11 10:44`
    pub fn ymd_hm(&self) -> String {
        self.naive.format("%Y-%m-%d %H:%M").to_string()
    }

    /// `2026-08-11`
    pub fn ymd(&self) -> String {
        self.naive.format("%Y-%m-%d").to_string()
    }

    /// English weekday name, e.g. `Tuesday`.
    pub fn weekday(&self) -> String {
        self.naive.weekday().to_string()
    }
}

/// Current time in the configured zone; `None` follows the host local zone.
pub fn now(tz: &Option<String>) -> Now {
    match resolve_tz(tz) {
        Some(z) => {
            let dt = Utc::now().with_timezone(&z);
            Now {
                naive: dt.naive_local(),
                zone_label: z.name().to_string(),
            }
        }
        None => {
            let dt = Local::now();
            Now {
                naive: dt.naive_local(),
                zone_label: "system local".to_string(),
            }
        }
    }
}

/// Unix epoch seconds. Timezone-independent by definition — use this for
/// elapsed-time arithmetic (idle gates, TTLs), never `now()`.
pub fn unix_now() -> i64 {
    Utc::now().timestamp()
}

/// Parse an IANA zone name, caching results.
///
/// Returns `None` for unset *and* for invalid names: an unparseable zone must
/// degrade to host local time rather than break the turn.
pub fn resolve_tz(tz: &Option<String>) -> Option<Tz> {
    let name = tz.as_deref()?.trim();
    if name.is_empty() {
        return None;
    }
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Tz>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    // Lock poisoning here would only mean a previous parse panicked (it cannot);
    // recover the guard instead of propagating a panic into every turn.
    let mut map = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(hit) = map.get(name) {
        return *hit;
    }
    let parsed = name.parse::<Tz>().ok();
    if parsed.is_none() {
        tracing::warn!(
            timezone = name,
            "unknown IANA timezone, falling back to system local"
        );
    }
    map.insert(name.to_string(), parsed);
    parsed
}

/// Whether a zone name is a valid IANA identifier (for config validation).
pub fn is_valid_tz(name: &str) -> bool {
    name.parse::<Tz>().is_ok()
}

/// The per-turn "status bar" appended to the message list (ADR-0017 §4).
///
/// Models have no clock: the system prompt is built once at process start, so a
/// long-running daemon would otherwise keep asserting yesterday's date. This
/// line is recomputed every turn and appended *after* the history, which keeps
/// the system prefix byte-identical across turns and therefore KV-cache
/// friendly.
pub fn status_bar(tz: &Option<String>) -> String {
    let n = now(tz);
    format!(
        "[Runtime status] Current time: {} {} ({}). {}\n\
         This line is regenerated every turn and is the authoritative clock — \
         prefer it over dates mentioned earlier in the conversation or in your training data.",
        n.ymd_hm(),
        n.weekday(),
        n.zone_label,
        day_hint(n.naive.hour())
    )
}

/// Coarse time-of-day hint so the model can pick an appropriate register
/// (ADR-0017 §2, "operational hint" rather than a bare timestamp).
fn day_hint(hour: u32) -> &'static str {
    match hour {
        0..=5 => "It is late night for the user — assume they are asleep unless they just spoke.",
        6..=8 => "Early morning.",
        9..=11 => "Morning working hours.",
        12..=13 => "Midday break.",
        14..=17 => "Afternoon working hours.",
        18..=22 => "Evening, off working hours.",
        _ => "Late evening.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_tz_valid() {
        let tz = resolve_tz(&Some("Asia/Shanghai".to_string()));
        assert!(tz.is_some());
        assert_eq!(tz.unwrap().name(), "Asia/Shanghai");
    }

    #[test]
    fn test_resolve_tz_invalid() {
        assert!(resolve_tz(&Some("Mars/Phobos".to_string())).is_none());
    }

    #[test]
    fn test_resolve_tz_unset_and_blank() {
        assert!(resolve_tz(&None).is_none());
        assert!(resolve_tz(&Some("   ".to_string())).is_none());
    }

    #[test]
    fn test_resolve_tz_cached_repeat() {
        // Second lookup must hit the cache and stay consistent.
        let a = resolve_tz(&Some("Europe/Paris".to_string()));
        let b = resolve_tz(&Some("Europe/Paris".to_string()));
        assert_eq!(a.map(|t| t.name()), b.map(|t| t.name()));
    }

    #[test]
    fn test_now_zone_label() {
        let n = now(&Some("Asia/Tokyo".to_string()));
        assert_eq!(n.zone_label, "Asia/Tokyo");
        assert_eq!(n.ymd().len(), 10);
        let local = now(&None);
        assert_eq!(local.zone_label, "system local");
    }

    #[test]
    fn test_now_respects_configured_zone() {
        // Tokyo is UTC+9, Honolulu UTC-10: 19 hours apart, so the two readings
        // can never share the same hour-of-day.
        let tokyo = now(&Some("Asia/Tokyo".to_string()));
        let honolulu = now(&Some("Pacific/Honolulu".to_string()));
        assert_ne!(tokyo.naive.hour(), honolulu.naive.hour());
    }

    #[test]
    fn test_status_bar_contains_zone_and_time() {
        let s = status_bar(&Some("Asia/Shanghai".to_string()));
        assert!(s.contains("Asia/Shanghai"));
        assert!(s.contains("[Runtime status]"));
        let today = now(&Some("Asia/Shanghai".to_string())).ymd();
        assert!(s.contains(&today));
    }

    #[test]
    fn test_status_bar_falls_back_on_invalid_zone() {
        let s = status_bar(&Some("Nowhere/Nothing".to_string()));
        assert!(s.contains("system local"));
    }

    #[test]
    fn test_is_valid_tz() {
        assert!(is_valid_tz("UTC"));
        assert!(is_valid_tz("America/New_York"));
        assert!(!is_valid_tz("Asia/Shanghai/Extra"));
    }

    #[test]
    fn test_unix_now_is_sane() {
        // Sanity floor: 2026-01-01. Guards against a broken clock source.
        assert!(unix_now() > 1_767_225_600);
    }

    #[test]
    fn test_day_hint_covers_all_hours() {
        for h in 0..24 {
            assert!(!day_hint(h).is_empty());
        }
    }
}
