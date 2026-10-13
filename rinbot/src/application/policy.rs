use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccessPolicy {
    pub allow_groups: Vec<String>,
    pub allow_private: Vec<String>,
    pub deny_groups: Vec<String>,
    pub deny_private: Vec<String>,
    pub allow_all_when_empty: bool,
    #[serde(default)]
    pub blocked_group_users: std::collections::BTreeMap<String, Vec<String>>,
}

impl AccessPolicy {
    pub fn allows(&self, is_group: bool, peer_id: &str) -> bool {
        let peer_id = peer_id.trim();
        let (allow, deny) = if is_group {
            (&self.allow_groups, &self.deny_groups)
        } else {
            (&self.allow_private, &self.deny_private)
        };
        if deny.iter().any(|value| value == peer_id) {
            return false;
        }
        self.allow_all_when_empty && allow.is_empty() || allow.iter().any(|value| value == peer_id)
    }

    pub fn blocks_sender(&self, conversation_id: &str, sender_id: &str) -> bool {
        let Some(group_id) = conversation_id.strip_prefix("group:") else {
            return false;
        };
        self.blocked_group_users
            .get(group_id)
            .is_some_and(|blocked| blocked.iter().any(|id| id == sender_id))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextTriggerConfig {
    pub random_percent: f64,
    pub keywords: Vec<String>,
    pub at_count: usize,
    pub keyword_count: usize,
    pub random_count: usize,
    pub all_count: usize,
    #[serde(default = "default_unified_tier")]
    pub unified_tier: bool,
    #[serde(default)]
    pub group_random_percent: std::collections::BTreeMap<String, f64>,
}

fn default_unified_tier() -> bool {
    true
}

impl ContextTriggerConfig {
    pub fn random_percent_for(&self, conversation_id: &str, is_group: bool) -> f64 {
        if !self.unified_tier && is_group {
            if let Some(group_id) = conversation_id.strip_prefix("group:") {
                if let Some(probability) = self.group_random_percent.get(group_id) {
                    return *probability;
                }
            }
        }
        self.random_percent
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BehaviorSettings {
    pub access: AccessPolicy,
    pub triggers: ContextTriggerConfig,
}

impl BehaviorSettings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.access.allow_all_when_empty {
            return Err("禁止空名单时自动放开访问；请明确填写 QQ 白名单");
        }
        for id in self
            .access
            .allow_groups
            .iter()
            .chain(&self.access.allow_private)
            .chain(&self.access.deny_groups)
            .chain(&self.access.deny_private)
        {
            if id.is_empty() || id.len() > 15 || !id.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err("访问名单只接受 1 至 15 位数字 QQ 号");
            }
        }
        if self.access.allow_groups.len() > 5_000
            || self.access.allow_private.len() > 5_000
            || self.access.deny_groups.len() > 5_000
            || self.access.deny_private.len() > 5_000
        {
            return Err("每份访问名单最多 5000 个 QQ 号");
        }
        if self.access.blocked_group_users.len() > 5_000
            || self
                .access
                .blocked_group_users
                .iter()
                .any(|(group, users)| {
                    !valid_qq_id(group)
                        || users.len() > 5_000
                        || users.iter().any(|user| !valid_qq_id(user))
                })
        {
            return Err("群成员屏蔽名单的群号和成员 QQ 号必须为数字，每群最多 5000 人");
        }
        if !self.triggers.random_percent.is_finite()
            || !(0.0..=100.0).contains(&self.triggers.random_percent)
        {
            return Err("群聊随机响应概率必须在 0 到 100 之间");
        }
        if self.triggers.keywords.len() > 128
            || self
                .triggers
                .keywords
                .iter()
                .any(|keyword| keyword.trim().is_empty() || keyword.chars().count() > 80)
        {
            return Err("触发关键词最多 128 个，每个需为 1 至 80 个字符");
        }
        if [
            self.triggers.at_count,
            self.triggers.keyword_count,
            self.triggers.random_count,
            self.triggers.all_count,
        ]
        .iter()
        .any(|count| *count > 500)
        {
            return Err("各触发类型上下文条数上限为 500");
        }
        if self.triggers.group_random_percent.len() > 5_000
            || self
                .triggers
                .group_random_percent
                .iter()
                .any(|(group, probability)| {
                    !valid_qq_id(group)
                        || !probability.is_finite()
                        || !(0.0..=100.0).contains(probability)
                })
        {
            return Err("分群响应概率必须使用数字群号，且为 0 到 100");
        }
        Ok(())
    }
}

fn valid_qq_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 15 && id.bytes().all(|byte| byte.is_ascii_digit())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriggerEvidence {
    pub text: String,
    pub mentions_self: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextDecision {
    pub tier: u8,
    pub count: usize,
    pub reason: &'static str,
    pub should_respond: bool,
}

/// Mirrors the legacy trigger priority while keeping random outcomes reproducible:
/// self-mention > keyword > full response > sampled response > ignore.
pub fn resolve_context_trigger(
    evidence: &[TriggerEvidence],
    config: &ContextTriggerConfig,
    roll_percent: f64,
) -> ContextDecision {
    if evidence.iter().any(|entry| entry.mentions_self) {
        return decision(1, config.at_count, "被艾特", true);
    }
    let keywords: Vec<String> = config
        .keywords
        .iter()
        .map(|keyword| keyword.trim().to_lowercase())
        .filter(|keyword| !keyword.is_empty())
        .collect();
    if evidence.iter().any(|entry| {
        let text = entry.text.to_lowercase();
        keywords.iter().any(|keyword| text.contains(keyword))
    }) {
        return decision(2, config.keyword_count, "关键词命中", true);
    }
    let probability = if config.random_percent.is_finite() {
        config.random_percent.clamp(0.0, 100.0)
    } else {
        0.0
    };
    if probability >= 100.0 {
        return decision(4, config.all_count, "全部响应", true);
    }
    if probability > 0.0 && roll_percent.is_finite() && roll_percent.clamp(0.0, 100.0) < probability
    {
        return decision(3, config.random_count, "随机命中", true);
    }
    decision(0, 0, "未触发", false)
}

fn decision(tier: u8, count: usize, reason: &'static str, should_respond: bool) -> ContextDecision {
    ContextDecision {
        tier,
        count,
        reason,
        should_respond,
    }
}

#[cfg(test)]
mod trigger_tests {
    use super::*;

    fn config() -> ContextTriggerConfig {
        ContextTriggerConfig {
            random_percent: 15.0,
            keywords: vec!["帮助".into()],
            at_count: 80,
            keyword_count: 50,
            random_count: 30,
            all_count: 100,
            unified_tier: true,
            group_random_percent: Default::default(),
        }
    }

    #[test]
    fn group_overrides_apply_only_when_tiers_are_not_unified() {
        let mut cfg = config();
        cfg.group_random_percent.insert("200".into(), 0.0);
        assert_eq!(cfg.random_percent_for("group:200", true), 15.0);
        cfg.unified_tier = false;
        assert_eq!(cfg.random_percent_for("group:200", true), 0.0);
        assert_eq!(cfg.random_percent_for("group:201", true), 15.0);
        assert_eq!(cfg.random_percent_for("private:100", false), 15.0);
    }

    #[test]
    fn group_member_blocklist_is_scoped_to_exact_group_and_sender() {
        let policy = AccessPolicy {
            blocked_group_users: [("200".into(), vec!["12345".into()])].into_iter().collect(),
            ..AccessPolicy::default()
        };
        assert!(policy.blocks_sender("group:200", "12345"));
        assert!(!policy.blocks_sender("group:201", "12345"));
        assert!(!policy.blocks_sender("private:200", "12345"));
        assert!(!policy.blocks_sender("group:200", "123456"));
    }

    #[test]
    fn mention_and_keyword_override_random_probability_with_separate_context_sizes() {
        let cfg = config();
        let mention = resolve_context_trigger(
            &[TriggerEvidence {
                text: "普通消息".into(),
                mentions_self: true,
            }],
            &cfg,
            99.0,
        );
        assert_eq!(
            (mention.tier, mention.count, mention.reason),
            (1, 80, "被艾特")
        );
        let keyword = resolve_context_trigger(
            &[TriggerEvidence {
                text: "请帮助我".into(),
                mentions_self: false,
            }],
            &cfg,
            99.0,
        );
        assert_eq!(
            (keyword.tier, keyword.count, keyword.reason),
            (2, 50, "关键词命中")
        );
    }

    #[test]
    fn random_probability_is_strict_and_reproducible_and_full_response_uses_its_own_count() {
        let cfg = config();
        assert_eq!(resolve_context_trigger(&[], &cfg, 14.999).count, 30);
        assert!(!resolve_context_trigger(&[], &cfg, 15.0).should_respond);
        let all = ContextTriggerConfig {
            random_percent: 100.0,
            ..cfg
        };
        let decision = resolve_context_trigger(&[], &all, 100.0);
        assert_eq!(
            (decision.tier, decision.count, decision.reason),
            (4, 100, "全部响应")
        );
    }

    #[test]
    fn empty_keywords_and_invalid_probability_do_not_create_false_triggers() {
        let cfg = ContextTriggerConfig {
            random_percent: f64::NAN,
            keywords: vec!["  ".into()],
            ..config()
        };
        assert!(
            !resolve_context_trigger(
                &[TriggerEvidence {
                    text: "help".into(),
                    mentions_self: false
                }],
                &cfg,
                0.0
            )
            .should_respond
        );
    }
}
