//! Safe source-reference continuity for image-aware handoffs.
//!
//! This feature stores only bounded QQ message/sticker identifiers. Image
//! bytes, data URLs, NapCat paths, and temporary media URLs never enter the
//! handoff or provider transcript.

use serde::{Deserialize, Serialize};

use crate::application::handoff::{MultimodalSource, MultimodalSourceKind};

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MultimodalContextSettings {
    pub enabled: bool,
    pub graduated: bool,
}

pub const MAX_MULTIMODAL_SOURCES: usize = 8;

pub fn append_source(
    sources: &mut Vec<MultimodalSource>,
    kind: MultimodalSourceKind,
    id: &str,
) -> bool {
    let id = id.trim();
    if id.is_empty() || id.chars().count() > 128 || id.chars().any(char::is_control) {
        return false;
    }
    let source = MultimodalSource {
        kind,
        id: id.to_owned(),
    };
    if sources.contains(&source) {
        return true;
    }
    if sources.len() == MAX_MULTIMODAL_SOURCES {
        sources.remove(0);
    }
    sources.push(source);
    true
}

pub fn merge_sources(
    previous: &[MultimodalSource],
    current: &[MultimodalSource],
) -> Vec<MultimodalSource> {
    let mut merged = Vec::new();
    for source in previous.iter().chain(current) {
        let _ = append_source(&mut merged, source.kind, &source.id);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_off_and_graduation_is_independent() {
        let settings = MultimodalContextSettings::default();
        assert!(!settings.enabled);
        assert!(!settings.graduated);
        let restored: MultimodalContextSettings =
            serde_json::from_value(serde_json::json!({"enabled":true,"graduated":false})).unwrap();
        assert!(restored.enabled);
        assert!(!restored.graduated);
    }

    #[test]
    fn source_merge_deduplicates_bounds_history_and_rejects_untrusted_ids() {
        let mut sources = Vec::new();
        assert!(append_source(
            &mut sources,
            MultimodalSourceKind::Message,
            " 123 "
        ));
        assert!(append_source(
            &mut sources,
            MultimodalSourceKind::Message,
            "123"
        ));
        assert!(!append_source(
            &mut sources,
            MultimodalSourceKind::Message,
            "bad\nid"
        ));
        for id in 0..10 {
            assert!(append_source(
                &mut sources,
                MultimodalSourceKind::Message,
                &id.to_string()
            ));
        }
        assert_eq!(sources.len(), MAX_MULTIMODAL_SOURCES);
        assert_eq!(sources.first().unwrap().id, "2");
        let merged = merge_sources(&sources, &[sources.last().unwrap().clone()]);
        assert_eq!(merged, sources);
    }
}
