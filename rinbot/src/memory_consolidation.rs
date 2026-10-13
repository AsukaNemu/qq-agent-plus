//! Conservative, deterministic rules shared by automatic and manual memory
//! consolidation. This module deliberately has no persistence or model side
//! effects; callers must validate a complete model response before writing.

use serde::Deserialize;
use thiserror::Error;

const DEFAULT_MIN_IMPRESSIONS: usize = 4;
const DEFAULT_MAX_PER_MEMBER: usize = 5;
const MAX_IMPRESSIONS_PER_MEMBER: usize = 1000;
const MAX_IMPRESSION_CHARS: usize = 120;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConsolidationError {
    #[error("模型结果不是有效 JSON")]
    InvalidJson,
    #[error("模型结果缺少 impressions 字符串数组")]
    InvalidShape,
    #[error("整理结果疑似引入新事实：{0}")]
    UnsafeGrowth(String),
}

#[derive(Debug, Deserialize)]
struct ModelResult {
    impressions: Vec<String>,
}

/// Node parity: run once if the chat has no impressions (so active people can
/// be discovered), otherwise run only after either configured threshold trips.
pub fn should_auto_consolidate(
    impression_count: usize,
    member_counts: &[usize],
    min_impressions: Option<usize>,
    max_per_member: Option<usize>,
) -> bool {
    if impression_count == 0 {
        return true;
    }
    let total_limit = min_impressions
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MIN_IMPRESSIONS)
        .max(1);
    if impression_count > total_limit {
        return true;
    }
    let member_limit = max_per_member
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MAX_PER_MEMBER)
        .max(2);
    member_counts.iter().any(|count| *count > member_limit)
}

/// Parse and validate an entire model response before it can be applied.
/// Malformed/non-string results and suspicious growth are rejected, never
/// interpreted as an empty list that could erase an existing impression set.
pub fn validate_model_result(
    response: &str,
    existing: &[String],
    is_new: bool,
    max_keep: usize,
) -> Result<Vec<String>, ConsolidationError> {
    let parsed = parse_model_result(response)?;

    if !is_new {
        validate_existing_growth(existing, &parsed.impressions)?;
    }

    Ok(parsed
        .impressions
        .into_iter()
        .map(|item| item.trim().to_owned())
        .filter(|item| !item.is_empty())
        .take(max_keep.clamp(1, MAX_IMPRESSIONS_PER_MEMBER))
        .map(|item| item.chars().take(MAX_IMPRESSION_CHARS).collect())
        .collect())
}

/// Node parity: models sometimes wrap an otherwise valid result in prose or a
/// Markdown fence. Extract only a JSON object and repair the two bounded syntax
/// mistakes Node tolerates (trailing commas and single-quoted strings); the
/// resulting value still has to deserialize to the strict `ModelResult` shape.
fn parse_model_result(response: &str) -> Result<ModelResult, ConsolidationError> {
    let text = response.trim();
    if text.is_empty() {
        return Err(ConsolidationError::InvalidJson);
    }
    let mut candidates = Vec::new();
    candidates.push(text.to_owned());

    if let Some(start) = text.find("```") {
        let after_fence = &text[start + 3..];
        let body_start = after_fence.find('\n').map(|index| index + 1).unwrap_or(0);
        if let Some(end) = after_fence[body_start..].find("```") {
            candidates.push(after_fence[body_start..body_start + end].trim().to_owned());
        }
    }

    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) {
        if end > start {
            candidates.push(text[start..=end].to_owned());
        }
    }

    let mut saw_json_shape_error = false;
    for candidate in candidates {
        if let Ok(parsed) = serde_json::from_str::<ModelResult>(&candidate) {
            return Ok(parsed);
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&candidate) {
            if serde_json::from_value::<ModelResult>(value).is_err() {
                saw_json_shape_error = true;
            }
        }
        let fixed = repair_common_json_syntax(&candidate);
        if fixed != candidate {
            if let Ok(parsed) = serde_json::from_str::<ModelResult>(&fixed) {
                return Ok(parsed);
            }
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&fixed) {
                if serde_json::from_value::<ModelResult>(value).is_err() {
                    saw_json_shape_error = true;
                }
            }
        }
    }
    Err(if saw_json_shape_error {
        ConsolidationError::InvalidShape
    } else {
        ConsolidationError::InvalidJson
    })
}

fn repair_common_json_syntax(candidate: &str) -> String {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Quote {
        None,
        Double,
        Single,
    }

    let mut output = String::with_capacity(candidate.len());
    let mut quote = Quote::None;
    let mut escaped = false;
    let chars = candidate.chars().collect::<Vec<_>>();
    for (index, ch) in chars.iter().copied().enumerate() {
        match quote {
            Quote::Double => {
                output.push(ch);
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    quote = Quote::None;
                }
            }
            Quote::Single => {
                if escaped {
                    if ch == '\'' || ch == '"' {
                        output.push(ch);
                    } else {
                        output.push('\\');
                        output.push(ch);
                    }
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '\'' {
                    output.push('"');
                    quote = Quote::None;
                } else if ch == '"' {
                    output.push_str("\\\"");
                } else {
                    output.push(ch);
                }
            }
            Quote::None => {
                if ch == '"' {
                    quote = Quote::Double;
                    output.push(ch);
                } else if ch == '\'' {
                    quote = Quote::Single;
                    output.push('"');
                } else if ch == ','
                    && chars[index + 1..]
                        .iter()
                        .copied()
                        .find(|next| !next.is_whitespace())
                        .is_some_and(|next| matches!(next, '}' | ']'))
                {
                    // Drop a trailing comma outside strings.
                } else {
                    output.push(ch);
                }
            }
        }
    }
    output
}

fn validate_existing_growth(
    existing: &[String],
    proposed: &[String],
) -> Result<(), ConsolidationError> {
    // Node checks the untrimmed, uncapped model output before safeSlice/maxKeep.
    // Count UTF-16 code units to match JavaScript String.length exactly.
    let before_chars: usize = existing
        .iter()
        .map(|item| item.encode_utf16().count())
        .sum();
    let after_chars: usize = proposed
        .iter()
        .map(|item| item.encode_utf16().count())
        .sum();
    if proposed.len() <= existing.len() {
        if before_chars > 0 && after_chars > before_chars.saturating_mul(2).saturating_add(80) {
            return Err(ConsolidationError::UnsafeGrowth(format!(
                "结果字数暴涨（{before_chars}→{after_chars} 字）"
            )));
        }
        return Ok(());
    }

    let growth = after_chars.saturating_sub(before_chars);
    let permitted_growth = 80usize.max((before_chars * 20 + 50) / 100);
    if proposed.len() == existing.len() + 1 && growth <= permitted_growth {
        return Ok(());
    }

    Err(ConsolidationError::UnsafeGrowth(format!(
        "结果变多（{}→{} 条、{before_chars}→{after_chars} 字）",
        existing.len(),
        proposed.len()
    )))
}

#[cfg(test)]
mod tests {
    use super::{should_auto_consolidate, validate_model_result, ConsolidationError};

    #[test]
    fn automatic_thresholds_match_node_and_allow_zero_memory_discovery() {
        assert!(should_auto_consolidate(0, &[], None, None));
        assert!(!should_auto_consolidate(4, &[5], None, None));
        assert!(should_auto_consolidate(5, &[], None, None));
        assert!(should_auto_consolidate(4, &[6], None, None));
        assert!(!should_auto_consolidate(4, &[5], Some(4), Some(5)));
        assert!(should_auto_consolidate(5, &[], Some(4), Some(5)));
        assert!(should_auto_consolidate(1, &[3], Some(0), Some(1)));
    }

    #[test]
    fn refuses_malformed_or_non_string_results_instead_of_erasing_memory() {
        let existing = vec!["喜欢羽毛球".to_owned()];
        assert_eq!(
            validate_model_result("not json", &existing, false, 5),
            Err(ConsolidationError::InvalidJson)
        );
        assert_eq!(
            validate_model_result(r#"{"impressions":[12]}"#, &existing, false, 5),
            Err(ConsolidationError::InvalidShape)
        );
        assert_eq!(
            validate_model_result(r#"{"result":[]}"#, &existing, false, 5),
            Err(ConsolidationError::InvalidShape)
        );
    }

    #[test]
    fn extracts_node_compatible_wrapped_memory_json_without_weakening_shape_checks() {
        let existing = vec!["喜欢羽毛球".to_owned()];
        for response in [
            "```json\n{\"impressions\":[\"喜欢打羽毛球\"]}\n```",
            "整理结果如下：{\"impressions\":[\"喜欢打羽毛球\"]}以上。",
            "{'impressions':['喜欢打羽毛球',],}",
        ] {
            assert_eq!(
                validate_model_result(response, &existing, false, 5).unwrap(),
                ["喜欢打羽毛球"]
            );
        }
        assert_eq!(
            validate_model_result("```json\n{\"result\":[]}\n```", &existing, false, 5),
            Err(ConsolidationError::InvalidShape)
        );
    }

    #[test]
    fn allows_compaction_and_a_small_split_but_rejects_suspicious_growth() {
        let existing = vec!["喜欢运动".to_owned(), "经常打羽毛球".to_owned()];
        assert_eq!(
            validate_model_result(r#"{"impressions":["喜欢打羽毛球"]}"#, &existing, false, 5)
                .unwrap(),
            ["喜欢打羽毛球"]
        );
        assert!(validate_model_result(
            r#"{"impressions":["喜欢运动","经常打羽毛球","喜欢周末和朋友打球"]}"#,
            &existing,
            false,
            5
        )
        .is_ok());
        assert!(matches!(
            validate_model_result(
                r#"{"impressions":["喜欢运动","经常打羽毛球","喜欢周末和朋友打球","住在上海"]}"#,
                &existing,
                false,
                5
            ),
            Err(ConsolidationError::UnsafeGrowth(_))
        ));
    }

    #[test]
    fn existing_memory_guard_checks_raw_model_output_before_truncation() {
        let existing = vec!["x".repeat(20)];
        let overlong = format!(r#"{{"impressions":["{}"]}}"#, "y".repeat(201));
        assert!(matches!(
            validate_model_result(&overlong, &existing, false, 5),
            Err(ConsolidationError::UnsafeGrowth(_))
        ));

        let existing = vec!["a".into(), "b".into(), "c".into(), "d".into(), "e".into()];
        let too_many = r#"{"impressions":["a","b","c","d","e","f","g"]}"#;
        assert!(matches!(
            validate_model_result(too_many, &existing, false, 5),
            Err(ConsolidationError::UnsafeGrowth(_))
        ));
    }

    #[test]
    fn existing_memory_growth_uses_node_utf16_character_count() {
        let existing = vec!["😀".repeat(20)];
        let proposed = format!(
            r#"{{"impressions":["{}","{}"]}}"#,
            "😀".repeat(20),
            "😁".repeat(41)
        );
        // JS sees 40 -> 122 code units (+82), above the +80 allowance.
        assert!(matches!(
            validate_model_result(&proposed, &existing, false, 5),
            Err(ConsolidationError::UnsafeGrowth(_))
        ));
    }

    #[test]
    fn new_memory_is_bounded_and_truncated_by_characters_not_utf8_bytes() {
        let response = format!(
            r#"{{"impressions":["  {}  ","","第二条","第三条","第四条","第五条","第六条"]}}"#,
            "中".repeat(130)
        );
        let result = validate_model_result(&response, &[], true, 5).unwrap();
        assert_eq!(result.len(), 5);
        assert_eq!(result[0].chars().count(), 120);
        assert_eq!(result[1], "第二条");
    }

    #[test]
    fn configured_member_limit_controls_saved_impression_count_with_a_hard_cap() {
        let response = r#"{"impressions":["一","二","三","四","五","六","七"]}"#;
        assert_eq!(
            validate_model_result(response, &[], true, 7).unwrap().len(),
            7
        );
        assert_eq!(
            validate_model_result(response, &[], true, 3).unwrap().len(),
            3
        );
        assert_eq!(
            validate_model_result(response, &[], true, 0).unwrap().len(),
            1
        );
        assert_eq!(
            validate_model_result(response, &[], true, 5000)
                .unwrap()
                .len(),
            7
        );
    }
}
