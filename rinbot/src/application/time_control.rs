use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

const MINUTES_PER_DAY: i64 = 1_440;
const MINUTES_PER_WEEK: i64 = 10_080;
const BEIJING_OFFSET_MINUTES: i64 = 480;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TimeControlConfig {
    pub enabled: bool,
    pub schedule: TimeSchedule,
    pub overrides: HashMap<String, TimeSchedule>,
}

impl Default for TimeControlConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            schedule: TimeSchedule::default(),
            overrides: HashMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TimeSchedule {
    pub mode: String,
    pub windows: Vec<TimeWindow>,
}

impl Default for TimeSchedule {
    fn default() -> Self {
        Self {
            mode: "deepseek-offpeak".into(),
            windows: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TimeWindow {
    pub days: Vec<u8>,
    pub start: String,
    pub end: String,
}

#[derive(Debug, Error)]
pub enum TimeControlError {
    #[error("invalid time-control JSON")]
    Json(#[from] serde_json::Error),
    #[error("invalid time-control schedule")]
    Schedule,
}

impl TimeControlConfig {
    pub fn parse(raw: &str) -> Result<Self, TimeControlError> {
        let config: Self = serde_json::from_str(raw)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), TimeControlError> {
        validate_schedule(&self.schedule)?;
        if self.overrides.len() > 1_000 {
            return Err(TimeControlError::Schedule);
        }
        for (chat_key, schedule) in &self.overrides {
            let Some((kind, id)) = chat_key.split_once(':') else {
                return Err(TimeControlError::Schedule);
            };
            if !matches!(kind, "group" | "private")
                || id.is_empty()
                || id.starts_with('0')
                || id.len() > 15
                || !id.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(TimeControlError::Schedule);
            }
            validate_schedule(schedule)?;
        }
        Ok(())
    }

    /// Evaluate the same weekly Asia/Shanghai schedule used by the Node runtime.
    /// Invalid schedules are rejected at startup; this method therefore fails closed.
    pub fn is_active(&self, chat_key: &str, timestamp_ms: i64) -> bool {
        if !self.enabled {
            return true;
        }
        let schedule = self
            .overrides
            .get(chat_key)
            .filter(|override_schedule| override_schedule.mode != "inherit")
            .unwrap_or(&self.schedule);
        match schedule.mode.as_str() {
            "always" => true,
            "deepseek-offpeak" => deepseek_offpeak(timestamp_ms),
            "custom" => custom_active(schedule, timestamp_ms),
            _ => false,
        }
    }

    pub fn next_active_at(&self, chat_key: &str, timestamp_ms: i64) -> Option<i64> {
        if self.is_active(chat_key, timestamp_ms) {
            return Some(timestamp_ms);
        }
        let next_minute = timestamp_ms.div_euclid(60_000).saturating_add(1);
        (0..=MINUTES_PER_WEEK).find_map(|offset| {
            let candidate = next_minute.saturating_add(offset).saturating_mul(60_000);
            self.is_active(chat_key, candidate).then_some(candidate)
        })
    }
}

fn validate_schedule(schedule: &TimeSchedule) -> Result<(), TimeControlError> {
    match schedule.mode.as_str() {
        "always" | "deepseek-offpeak" | "custom" => {
            if schedule.windows.len() > 32 {
                return Err(TimeControlError::Schedule);
            }
            for window in &schedule.windows {
                if window.days.is_empty() || window.days.iter().any(|day| !(1..=7).contains(day)) {
                    return Err(TimeControlError::Schedule);
                }
                let start = parse_time(&window.start, false).ok_or(TimeControlError::Schedule)?;
                let end = parse_time(&window.end, true).ok_or(TimeControlError::Schedule)?;
                if start == end {
                    return Err(TimeControlError::Schedule);
                }
            }
        }
        "inherit" => {
            if !schedule.windows.is_empty() {
                return Err(TimeControlError::Schedule);
            }
        }
        _ => return Err(TimeControlError::Schedule),
    }
    Ok(())
}

fn parse_time(value: &str, allow_24: bool) -> Option<i64> {
    if allow_24 && value == "24:00" {
        return Some(MINUTES_PER_DAY);
    }
    let (hour, minute) = value.split_once(':')?;
    if hour.len() != 2 || minute.len() != 2 {
        return None;
    }
    let hour = hour.parse::<i64>().ok()?;
    let minute = minute.parse::<i64>().ok()?;
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) {
        return None;
    }
    Some(hour * 60 + minute)
}

fn local_week_minute(timestamp_ms: i64) -> i64 {
    let local_minutes = timestamp_ms.div_euclid(60_000) + BEIJING_OFFSET_MINUTES;
    let epoch_day = local_minutes.div_euclid(MINUTES_PER_DAY);
    let minute_of_day = local_minutes.rem_euclid(MINUTES_PER_DAY);
    let monday_based_weekday = (epoch_day + 3).rem_euclid(7);
    monday_based_weekday * MINUTES_PER_DAY + minute_of_day
}

fn deepseek_offpeak(timestamp_ms: i64) -> bool {
    let minute = local_week_minute(timestamp_ms);
    let weekday = minute / MINUTES_PER_DAY;
    if weekday >= 5 {
        return true;
    }
    let hour = (minute % MINUTES_PER_DAY) / 60;
    !(9..12).contains(&hour) && !(14..18).contains(&hour)
}

fn custom_active(schedule: &TimeSchedule, timestamp_ms: i64) -> bool {
    let current = local_week_minute(timestamp_ms);
    schedule.windows.iter().any(|window| {
        let Some(start_minute) = parse_time(&window.start, false) else {
            return false;
        };
        let Some(end_minute) = parse_time(&window.end, true) else {
            return false;
        };
        window.days.iter().any(|day| {
            let start = (i64::from(*day) - 1) * MINUTES_PER_DAY + start_minute;
            let duration = if end_minute > start_minute {
                end_minute - start_minute
            } else {
                MINUTES_PER_DAY - start_minute + end_minute
            };
            let end = start + duration;
            [current, current + MINUTES_PER_WEEK].iter().any(|point| {
                *point >= start && *point < end
                    || *point + MINUTES_PER_WEEK >= start && *point + MINUTES_PER_WEEK < end
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONDAY_LOCAL_MIDNIGHT_2024_MS: i64 = 1_704_038_400_000;

    fn at(weekday: u8, hour: u8, minute: u8) -> i64 {
        MONDAY_LOCAL_MIDNIGHT_2024_MS
            + (i64::from(weekday - 1) * MINUTES_PER_DAY + i64::from(hour) * 60 + i64::from(minute))
                * 60_000
    }

    #[test]
    fn disabled_and_always_modes_are_active_without_timezone_drift() {
        assert!(TimeControlConfig::default().is_active("group:1", at(1, 10, 0)));
        let config = TimeControlConfig {
            enabled: true,
            schedule: TimeSchedule {
                mode: "always".into(),
                windows: Vec::new(),
            },
            overrides: HashMap::new(),
        };
        assert!(config.is_active("private:2", at(1, 10, 0)));
    }

    #[test]
    fn deepseek_offpeak_matches_weekday_peak_windows_and_weekends() {
        let config = TimeControlConfig {
            enabled: true,
            ..TimeControlConfig::default()
        };
        assert!(config.is_active("group:1", at(1, 8, 59)));
        assert!(!config.is_active("group:1", at(1, 9, 0)));
        assert!(!config.is_active("group:1", at(1, 11, 59)));
        assert!(config.is_active("group:1", at(1, 12, 0)));
        assert!(!config.is_active("group:1", at(5, 17, 59)));
        assert!(config.is_active("group:1", at(5, 18, 0)));
        assert!(config.is_active("group:1", at(6, 10, 0)));
    }

    #[test]
    fn custom_windows_support_overnight_and_per_conversation_override() {
        let config = TimeControlConfig::parse(
            r#"{"enabled":true,"schedule":{"mode":"custom","windows":[{"days":[1],"start":"22:00","end":"02:00"}]},"overrides":{"private:22":{"mode":"always","windows":[]}}}"#,
        )
        .unwrap();
        assert!(config.is_active("group:10", at(1, 23, 0)));
        assert!(config.is_active("group:10", at(2, 1, 59)));
        assert!(!config.is_active("group:10", at(2, 2, 0)));
        assert!(config.is_active("private:22", at(2, 12, 0)));
        assert!(!config.is_active("private:23", at(2, 12, 0)));
    }

    #[test]
    fn sunday_overnight_window_wraps_into_the_next_week() {
        let config = TimeControlConfig::parse(
            r#"{"enabled":true,"schedule":{"mode":"custom","windows":[{"days":[7],"start":"22:00","end":"02:00"}]},"overrides":{}}"#,
        )
        .unwrap();
        assert!(config.is_active("group:10", at(7, 23, 0)));
        assert!(config.is_active("group:10", at(1, 1, 59)));
        assert!(!config.is_active("group:10", at(1, 2, 0)));
    }

    #[test]
    fn invalid_time_schedules_are_rejected_at_load() {
        for json in [
            r#"{"enabled":true,"schedule":{"mode":"custom","windows":[{"days":[0],"start":"10:00","end":"11:00"}]}}"#,
            r#"{"enabled":true,"schedule":{"mode":"custom","windows":[{"days":[1],"start":"24:00","end":"01:00"}]}}"#,
            r#"{"enabled":true,"schedule":{"mode":"unknown","windows":[]}}"#,
        ] {
            assert!(TimeControlConfig::parse(json).is_err());
        }
    }

    #[test]
    fn next_active_at_points_to_the_next_configured_minute_boundary() {
        let config = TimeControlConfig::parse(
            r#"{"enabled":true,"schedule":{"mode":"custom","windows":[{"days":[2],"start":"10:15","end":"11:00"}]},"overrides":{}}"#,
        )
        .unwrap();
        let now = at(1, 9, 0) + 12_345;
        assert_eq!(config.next_active_at("group:1", now), Some(at(2, 10, 15)));
        assert_eq!(
            config.next_active_at("group:1", at(2, 10, 30)),
            Some(at(2, 10, 30))
        );
    }
}
