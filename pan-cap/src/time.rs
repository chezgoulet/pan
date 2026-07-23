//! # `cap.time` — current time information.
//!
//! Provides `cap.time.now` (current UTC datetime as ISO 8601) and `cap.time.today`
//! (current UTC date). Trivial but high-value — models love to hallucinate dates.

use pan_core::pipeline::ExecError;
use pan_core::schema::{Capability, Value};
use pan_core::toolbox::CapabilityProvider;

/// Time capability: returns current date/time in ISO 8601.
pub struct TimeCaps;

impl Default for TimeCaps {
    fn default() -> Self {
        Self
    }
}

impl TimeCaps {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl CapabilityProvider for TimeCaps {
    fn id(&self) -> &str {
        "cap.time"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability {
                id: "cap.time.now".into(),
                summary: "get the current UTC datetime as ISO 8601".into(),
                args_schema: serde_json::json!({ "type": "object" }),
            },
            Capability {
                id: "cap.time.today".into(),
                summary: "get the current UTC date as ISO 8601".into(),
                args_schema: serde_json::json!({ "type": "object" }),
            },
        ]
    }

    async fn execute(&self, capability: &str, _args: &Value) -> Result<Value, ExecError> {
        match capability {
            "cap.time.now" => {
                let now = chrono_now();
                Ok(serde_json::json!({ "datetime": now }))
            }
            "cap.time.today" => {
                let today = chrono_today();
                Ok(serde_json::json!({ "date": today }))
            }
            other => Err(ExecError(format!("cap.time has no `{other}`"))),
        }
    }
}

fn chrono_now() -> String {
    // Hand-rolled UTC timestamp to avoid pulling in chrono/humantime deps.
    // Format: 2026-07-19T12:34:56Z
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();

    // Days since epoch
    let days = secs / 86400;
    let secs_of_day = secs % 86400;
    let hours = secs_of_day / 3600;
    let minutes = (secs_of_day % 3600) / 60;
    let seconds = secs_of_day % 60;

    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, m, d, hours, minutes, seconds
    )
}

fn chrono_today() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let days = dur.as_secs() / 86400;
    let (y, m, d) = civil_from_days(days as i64);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// Convert a number of days since 1970-01-01 to (year, month, day).
/// Uses the civil_from_days algorithm from Howard Hinnant (public domain).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let m = m as u32;
    let d = d as u32;
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn now_returns_iso_8601() {
        let t = TimeCaps::new();
        let result = t.execute("cap.time.now", &Value::Null).await.unwrap();
        let datetime = result["datetime"].as_str().unwrap();
        // 2026-07-19T12:34:56Z format
        assert!(datetime.len() > 18, "got: {datetime}");
        assert!(datetime.ends_with('Z'), "must end with Z: {datetime}");
        assert!(datetime.contains('T'), "must have T separator: {datetime}");
    }

    #[tokio::test]
    async fn today_returns_date_only() {
        let t = TimeCaps::new();
        let result = t.execute("cap.time.today", &Value::Null).await.unwrap();
        let date = result["date"].as_str().unwrap();
        // YYYY-MM-DD
        assert_eq!(date.len(), 10, "got: {date}");
        assert!(!date.contains('T'));
    }

    #[test]
    fn civil_days_known_date() {
        // 1970-01-01 = day 0
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2024-01-01
        let unix_2024 = 1704067200 / 86400; // ~19723 days
        let (y, _m, _d) = civil_from_days(unix_2024 as i64);
        // Just verify it produces a plausible result
        assert!(y >= 2024, "year should be >=2024, got {y}");
    }
}
