//! Pure Shanghai-time planner for the Daily Moments feature.
//!
//! Slots are durable identities: replanning may cancel or miss pending work,
//! but it never redraws a slot that already exists or rearms a terminal slot.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const MOMENT_DAY_MS: i64 = 86_400_000;
pub const MOMENT_MIN_GAP_MS: i64 = 5 * 60_000;
const SHANGHAI_OFFSET_MS: i64 = 8 * 60 * 60 * 1000;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MomentScheduleWindow {
    pub start: String,
    pub end: String,
    pub count: u8,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MomentScheduleError {
    #[error("每日动态需要配置 1 至 8 个时间范围")]
    WindowCount,
    #[error("动态时间范围必须使用 HH:MM 格式")]
    Clock,
    #[error("每个时间范围的动态条数必须为 1 至 10")]
    SlotCount,
    #[error("每条动态至少预留 5 分钟；开始和结束时间不能相同")]
    Duration,
    #[error("每天最多安排 20 条定时动态")]
    DailyLimit,
    #[error("动态时间范围不能重叠（包括跨午夜范围）")]
    Overlap,
}

pub fn normalize_moment_windows(
    windows: Option<&[MomentScheduleWindow]>,
) -> Result<Option<Vec<MomentScheduleWindow>>, MomentScheduleError> {
    let Some(windows) = windows else {
        return Ok(None);
    };
    if windows.is_empty() || windows.len() > 8 {
        return Err(MomentScheduleError::WindowCount);
    }
    let mut minutes = Vec::with_capacity(windows.len());
    let mut total_count = 0u16;
    for window in windows {
        let start = clock_minutes(&window.start)?;
        let end = clock_minutes(&window.end)?;
        if !(1..=10).contains(&window.count) {
            return Err(MomentScheduleError::SlotCount);
        }
        let duration = (end - start + 1440) % 1440;
        if duration * 60_000 < i64::from(window.count) * MOMENT_MIN_GAP_MS {
            return Err(MomentScheduleError::Duration);
        }
        total_count += u16::from(window.count);
        minutes.push((start, end, duration));
    }
    if total_count > 20 {
        return Err(MomentScheduleError::DailyLimit);
    }
    let mut occupied = [false; 1440];
    for (start, _, duration) in &minutes {
        for offset in 0..*duration {
            let minute = ((*start + offset) % 1440) as usize;
            if occupied[minute] {
                return Err(MomentScheduleError::Overlap);
            }
            occupied[minute] = true;
        }
    }
    Ok(Some(windows.to_vec()))
}

pub fn update_moment_plan(
    slots: &mut Vec<Value>,
    windows: Option<&[MomentScheduleWindow]>,
    now: i64,
    mut random: impl FnMut() -> f64,
    startup: bool,
    catchup: bool,
) -> bool {
    let day_start = shanghai_day_start(now);
    let configured = windows
        .unwrap_or_default()
        .iter()
        .map(|window| (moment_window_key(window), window))
        .collect::<std::collections::HashMap<_, _>>();
    let mut changed = false;

    for slot in slots.iter_mut() {
        if text(slot, "status") != Some("pending") {
            continue;
        }
        let window_key = text(slot, "windowKey").unwrap_or_default();
        let index = integer(slot, "index").unwrap_or(i64::MAX);
        let Some(window) = configured.get(window_key) else {
            set_terminal(slot, "cancelled", "时间范围已修改或移除");
            changed = true;
            continue;
        };
        if index >= i64::from(window.count) {
            set_terminal(slot, "cancelled", "时间范围已修改或移除");
            changed = true;
            continue;
        }
        let end_at = integer(slot, "endAt").unwrap_or_default();
        let at = integer(slot, "at").unwrap_or_default();
        if now >= end_at || (startup && !catchup && at < now) {
            set_terminal(
                slot,
                "missed",
                if now >= end_at {
                    "已错过允许发布时间范围"
                } else {
                    "启动时已错过随机时间，未开启窗口内补跑"
                },
            );
            changed = true;
        }
    }

    let mut ids = slots
        .iter()
        .filter_map(|slot| text(slot, "id").map(str::to_owned))
        .collect::<std::collections::HashSet<_>>();
    for offset in [-1_i64, 0, 1] {
        let start_of_day = day_start + offset * MOMENT_DAY_MS;
        let day_key = shanghai_day_key(start_of_day);
        for window in windows.unwrap_or_default() {
            let window_key = moment_window_key(window);
            let start = clock_minutes(&window.start).unwrap_or_default();
            let end = clock_minutes(&window.end).unwrap_or_default();
            let start_at = start_of_day + i64::from(start) * 60_000;
            let end_at =
                start_of_day + i64::from(if end <= start { end + 1440 } else { end }) * 60_000;
            if offset == -1 && end_at <= now {
                continue;
            }
            let bucket = (end_at - start_at) / i64::from(window.count);
            for index in 0..window.count {
                let id = format!("{day_key}/{window_key}/{}", index + 1);
                if !ids.insert(id.clone()) {
                    continue;
                }
                let sample = random();
                let sample = if sample.is_finite() { sample } else { 0.0 };
                let sample = sample.clamp(0.0, 0.999_999_999);
                let at = start_at
                    + i64::from(index) * bucket
                    + (sample * (bucket - MOMENT_MIN_GAP_MS) as f64).floor() as i64;
                let missed = now >= end_at || (at < now && !catchup);
                slots.push(serde_json::json!({
                    "id": id,
                    "dayKey": day_key,
                    "windowKey": window_key,
                    "index": index,
                    "at": at,
                    "endAt": end_at,
                    "status": if missed { "missed" } else { "pending" },
                    "reason": if missed { "计划建立时已错过时间，未补跑" } else { "" },
                    "recordId": ""
                }));
                changed = true;
            }
        }
    }
    slots.sort_by(|left, right| {
        integer(left, "at")
            .unwrap_or_default()
            .cmp(&integer(right, "at").unwrap_or_default())
            .then_with(|| text(left, "id").cmp(&text(right, "id")))
    });
    let retained = slots
        .iter()
        .filter(|slot| integer(slot, "endAt").unwrap_or_default() >= day_start - 7 * MOMENT_DAY_MS)
        .cloned()
        .collect::<Vec<_>>();
    if retained.len() != slots.len() {
        *slots = retained;
        changed = true;
    }
    changed
}

/// Plan the legacy fixed-time mode without introducing random drift. During a
/// startup before today's target, Node's catch-up behavior assigns the missed
/// run to the previous Shanghai day; otherwise only today's due date is caught
/// up. Adjacent dates are retained to make restarts idempotent.
pub fn update_fixed_moment_plan(
    slots: &mut Vec<Value>,
    hour: u8,
    minute: u8,
    now: i64,
    startup: bool,
    catchup: bool,
) -> bool {
    let day_start = shanghai_day_start(now);
    let today_target = day_start + (i64::from(hour) * 60 + i64::from(minute)) * 60_000;
    let mut ids = slots
        .iter()
        .filter_map(|slot| text(slot, "id").map(str::to_owned))
        .collect::<std::collections::HashSet<_>>();
    let mut changed = false;
    let fixed_suffix = format!("/fixed/{hour:02}:{minute:02}");
    for slot in slots.iter_mut() {
        if text(slot, "status") != Some("pending") {
            continue;
        }
        let id = text(slot, "id").unwrap_or_default();
        let end_at = integer(slot, "endAt").unwrap_or_default();
        if !id.ends_with(&fixed_suffix) {
            set_terminal(slot, "cancelled", "固定时刻配置已修改");
            changed = true;
        } else if end_at <= now {
            set_terminal(slot, "missed", "已错过固定时刻补跑期限");
            changed = true;
        }
    }
    for offset in [-1_i64, 0, 1] {
        let target = today_target + offset * MOMENT_DAY_MS;
        let day_key = shanghai_day_key(day_start + offset * MOMENT_DAY_MS);
        let id = format!("{day_key}/fixed/{hour:02}:{minute:02}");
        if !ids.insert(id.clone()) {
            continue;
        }
        if offset < 0 && !(startup && catchup && now < today_target) {
            continue;
        }
        let missed_at_start = offset == 0 && startup && !catchup && now >= target;
        slots.push(serde_json::json!({
            "id": id,
            "dayKey": day_key,
            "windowKey": "fixed",
            "index": 0,
            "at": target,
            "endAt": target + MOMENT_DAY_MS,
            "status": if missed_at_start { "missed" } else { "pending" },
            "reason": if missed_at_start { "启动时已错过固定时刻，未开启补跑" } else { "" },
            "recordId": ""
        }));
        changed = true;
    }
    slots.sort_by(|left, right| {
        integer(left, "at")
            .unwrap_or_default()
            .cmp(&integer(right, "at").unwrap_or_default())
            .then_with(|| text(left, "id").cmp(&text(right, "id")))
    });
    let retained = slots
        .iter()
        .filter(|slot| integer(slot, "endAt").unwrap_or_default() >= day_start - 7 * MOMENT_DAY_MS)
        .cloned()
        .collect::<Vec<_>>();
    if retained.len() != slots.len() {
        *slots = retained;
        changed = true;
    }
    changed
}

pub fn moment_window_key(window: &MomentScheduleWindow) -> String {
    format!("{}-{}", window.start, window.end)
}

/// Revalidate a claimed automatic slot against the current settings immediately
/// before any external publication. Generation can take long enough for an
/// administrator to change the schedule while the model is working.
pub fn slot_matches_current_publish_plan(
    slot: &Value,
    schedule_windows: Option<&[MomentScheduleWindow]>,
    hour: u8,
    minute: u8,
    now: i64,
) -> bool {
    let Some(window_key) = text(slot, "windowKey") else {
        return false;
    };
    let Some(index) = integer(slot, "index") else {
        return false;
    };
    let Some(end_at) = integer(slot, "endAt") else {
        return false;
    };
    if now >= end_at {
        return false;
    }

    match schedule_windows {
        Some(windows) => windows.iter().any(|window| {
            window_key == moment_window_key(window) && index >= 0 && index < i64::from(window.count)
        }),
        None => {
            window_key == "fixed"
                && index == 0
                && text(slot, "id")
                    .is_some_and(|id| id.ends_with(&format!("/fixed/{hour:02}:{minute:02}")))
        }
    }
}

pub fn shanghai_day_start(now: i64) -> i64 {
    now.saturating_add(SHANGHAI_OFFSET_MS)
        .div_euclid(MOMENT_DAY_MS)
        .saturating_mul(MOMENT_DAY_MS)
        .saturating_sub(SHANGHAI_OFFSET_MS)
}

fn clock_minutes(value: &str) -> Result<i64, MomentScheduleError> {
    let bytes = value.as_bytes();
    if bytes.len() != 5
        || bytes[2] != b':'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 2 || byte.is_ascii_digit())
    {
        return Err(MomentScheduleError::Clock);
    }
    let hour: i64 = value[..2].parse().map_err(|_| MomentScheduleError::Clock)?;
    let minute: i64 = value[3..].parse().map_err(|_| MomentScheduleError::Clock)?;
    if hour > 23 || minute > 59 {
        return Err(MomentScheduleError::Clock);
    }
    Ok(hour * 60 + minute)
}

fn shanghai_day_key(start: i64) -> String {
    let days = start
        .saturating_add(SHANGHAI_OFFSET_MS)
        .div_euclid(MOMENT_DAY_MS);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

// Howard Hinnant's civil-from-days algorithm; epoch day zero is 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn integer(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

fn set_terminal(slot: &mut Value, status: &str, reason: &str) {
    if let Some(object) = slot.as_object_mut() {
        object.insert("status".into(), Value::String(status.into()));
        object.insert("reason".into(), Value::String(reason.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(clock: &str) -> i64 {
        let [hour, minute, second]: [i64; 3] = clock
            .split(':')
            .map(|part| part.parse().unwrap())
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        1_789_372_800_000 + (hour - 16) * 3_600_000 + minute * 60_000 + second * 1_000
    }

    fn windows() -> Vec<MomentScheduleWindow> {
        vec![MomentScheduleWindow {
            start: "17:00".into(),
            end: "18:00".into(),
            count: 3,
        }]
    }

    #[test]
    fn validates_window_shape_duration_overlap_and_daily_limit() {
        assert_eq!(normalize_moment_windows(None).unwrap(), None);
        assert_eq!(
            normalize_moment_windows(Some(&windows())).unwrap(),
            Some(windows())
        );
        for invalid in [
            vec![],
            vec![MomentScheduleWindow {
                start: "25:00".into(),
                end: "18:00".into(),
                count: 1,
            }],
            vec![MomentScheduleWindow {
                start: "17:00".into(),
                end: "17:00".into(),
                count: 1,
            }],
            vec![MomentScheduleWindow {
                start: "17:00".into(),
                end: "17:04".into(),
                count: 1,
            }],
            vec![MomentScheduleWindow {
                start: "17:00".into(),
                end: "18:00".into(),
                count: 11,
            }],
            vec![
                MomentScheduleWindow {
                    start: "23:30".into(),
                    end: "00:30".into(),
                    count: 1,
                },
                MomentScheduleWindow {
                    start: "00:00".into(),
                    end: "01:00".into(),
                    count: 1,
                },
            ],
        ] {
            assert!(
                normalize_moment_windows(Some(&invalid)).is_err(),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn creates_separated_slots_once_and_preserves_terminal_slots() {
        let windows = windows();
        let mut slots = Vec::new();
        let mut draws = 0;
        assert!(update_moment_plan(
            &mut slots,
            Some(&windows),
            at("16:00:00"),
            || {
                draws += 1;
                if draws % 2 == 1 {
                    0.999999
                } else {
                    0.0
                }
            },
            false,
            false
        ));
        assert_eq!(
            slots
                .iter()
                .filter(|slot| slot["dayKey"] == "2026-09-14")
                .count(),
            3
        );
        let before = slots.clone();
        assert!(!update_moment_plan(
            &mut slots,
            Some(&windows),
            at("16:30:00"),
            || { panic!("existing slots must not be redrawn") },
            false,
            false
        ));
        assert_eq!(slots, before);
        slots[0]["status"] = Value::String("published".into());
        update_moment_plan(
            &mut slots,
            Some(&windows[..0]),
            at("16:31:00"),
            || 0.0,
            false,
            false,
        );
        assert_eq!(slots[0]["status"], "published");
    }

    #[test]
    fn cross_midnight_and_startup_catchup_match_node_rules() {
        let window = [MomentScheduleWindow {
            start: "23:30".into(),
            end: "00:30".into(),
            count: 2,
        }];
        let mut slots = Vec::new();
        update_moment_plan(
            &mut slots,
            Some(&window),
            at("00:05:00"),
            || 0.0,
            false,
            true,
        );
        let previous = slots
            .iter()
            .filter(|slot| slot["dayKey"] == "2026-09-13")
            .collect::<Vec<_>>();
        assert_eq!(previous.len(), 2);
        assert_eq!(previous[1]["at"], at("00:00:00"));
        assert!(previous.iter().all(|slot| slot["status"] == "pending"));
        update_moment_plan(
            &mut slots,
            Some(&window),
            at("00:30:00"),
            || 0.0,
            false,
            false,
        );
        assert!(slots
            .iter()
            .filter(|slot| slot["dayKey"] == "2026-09-13")
            .all(|slot| slot["status"] == "missed"));

        let mut startup = Vec::new();
        update_moment_plan(
            &mut startup,
            Some(&windows()),
            at("16:00:00"),
            || 0.0,
            false,
            false,
        );
        update_moment_plan(
            &mut startup,
            Some(&windows()),
            at("17:05:00"),
            || 0.0,
            true,
            false,
        );
        assert_eq!(startup[0]["status"], "missed");
        let mut catchup = Vec::new();
        update_moment_plan(
            &mut catchup,
            Some(&windows()),
            at("16:00:00"),
            || 0.0,
            false,
            false,
        );
        update_moment_plan(
            &mut catchup,
            Some(&windows()),
            at("17:05:00"),
            || 0.0,
            true,
            true,
        );
        assert_eq!(catchup[0]["status"], "pending");
    }

    #[test]
    fn fixed_time_plan_keeps_exact_target_and_node_startup_catchup_semantics() {
        let now = at("16:00:00");
        let mut before_target = Vec::new();
        assert!(update_fixed_moment_plan(
            &mut before_target,
            17,
            30,
            now,
            true,
            true
        ));
        let previous = before_target
            .iter()
            .find(|slot| slot["at"].as_i64().unwrap() < now)
            .unwrap();
        let today = before_target
            .iter()
            .find(|slot| slot["at"].as_i64().unwrap() > now)
            .unwrap();
        assert_eq!(previous["status"], "pending");
        assert_eq!(today["at"], now + 90 * 60_000);

        let after_target = at("18:00:00");
        let mut no_catchup = Vec::new();
        update_fixed_moment_plan(&mut no_catchup, 17, 30, after_target, true, false);
        let missed_today = no_catchup
            .iter()
            .find(|slot| slot["at"].as_i64().unwrap() < after_target)
            .unwrap();
        assert_eq!(missed_today["status"], "missed");
        let next_day = no_catchup
            .iter()
            .find(|slot| slot["at"].as_i64().unwrap() > after_target)
            .unwrap();
        assert_eq!(next_day["status"], "pending");

        let count = no_catchup.len();
        assert!(!update_fixed_moment_plan(
            &mut no_catchup,
            17,
            30,
            after_target + 1_000,
            false,
            false
        ));
        assert_eq!(no_catchup.len(), count);
    }

    #[test]
    fn publication_rechecks_current_schedule_and_expiry() {
        let windows = [MomentScheduleWindow {
            start: "17:00".into(),
            end: "19:00".into(),
            count: 2,
        }];
        let slot = serde_json::json!({
            "id":"2026-09-14/17:00-19:00/2",
            "windowKey":"17:00-19:00",
            "index":1,
            "endAt":at("19:00:00")
        });
        assert!(slot_matches_current_publish_plan(
            &slot,
            Some(&windows),
            23,
            30,
            at("18:59:59")
        ));
        assert!(!slot_matches_current_publish_plan(
            &slot,
            Some(&windows[..0]),
            23,
            30,
            at("18:00:00")
        ));
        let reduced = [MomentScheduleWindow {
            count: 1,
            ..windows[0].clone()
        }];
        assert!(!slot_matches_current_publish_plan(
            &slot,
            Some(&reduced),
            23,
            30,
            at("18:00:00")
        ));
        assert!(!slot_matches_current_publish_plan(
            &slot,
            Some(&windows),
            23,
            30,
            at("19:00:00")
        ));
    }

    #[test]
    fn fixed_publication_rechecks_time_and_mode_changes() {
        let slot = serde_json::json!({
            "id":"2026-09-14/fixed/23:30",
            "windowKey":"fixed",
            "index":0,
            "endAt":at("23:30:00") + MOMENT_DAY_MS
        });
        assert!(slot_matches_current_publish_plan(
            &slot,
            None,
            23,
            30,
            at("23:31:00")
        ));
        assert!(!slot_matches_current_publish_plan(
            &slot,
            None,
            22,
            30,
            at("23:31:00")
        ));
        assert!(!slot_matches_current_publish_plan(
            &slot,
            Some(&windows()),
            23,
            30,
            at("23:31:00")
        ));
    }
}
