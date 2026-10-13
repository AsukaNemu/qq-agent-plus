//! Read-only inventory of a legacy Node data directory before migration.
//! The report intentionally contains schema and aggregate counts only.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs,
    io::{Read, Write},
    path::Path,
};

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;

const KNOWN_DATABASES: &[&str] = &[
    "messages.sqlite",
    "identity-pilot.sqlite",
    "relationship-pilot.sqlite",
    "incident-pilot.sqlite",
    "slang-pilot.sqlite",
];
const STATE_VALUES: &[&str] = &[
    "active",
    "acked",
    "cancelled",
    "completed",
    "done",
    "error",
    "failed",
    "held",
    "leased",
    "pending",
    "paused",
    "processing",
    "queued",
    "reconciled_failed",
    "reconciled_sent",
    "sending",
    "sent",
    "unknown",
];
const DATA_PATHS: &[(&str, &[&str])] = &[
    ("memory", &["memory"]),
    ("sessions", &["sessions"]),
    // Node historically stores the sticker catalog and its media as siblings
    // in the data root; newer layouts may use a stickers/ directory.
    ("stickers", &["stickers", "stickers.json", "sticker-assets"]),
    ("assets", &["assets"]),
    ("media", &["media"]),
    ("qzone", &["qzone"]),
    ("tasks", &["tasks"]),
    ("backups", &["backups"]),
];

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MigrationAudit {
    pub format_version: u32,
    pub databases: Vec<DatabaseAudit>,
    pub directories: Vec<DirectoryAudit>,
    pub memory_archive: MemoryArchiveAudit,
    pub session_archive: SessionArchiveAudit,
    pub sticker_archive: StickerArchiveAudit,
    pub archives: Vec<JsonArchiveAudit>,
    pub config: Option<ConfigAudit>,
    pub failures: Vec<AuditFailure>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SessionArchiveAudit {
    pub present: bool,
    /// Direct JSON files considered by the legacy-session importer.
    pub source_json_files: u64,
    pub importable_json_files: u64,
    pub invalid_json_files: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MemoryArchiveAudit {
    pub present: bool,
    pub source_json_files: u64,
    pub parseable_json_files: u64,
    pub source_person_files: u64,
    pub importable_person_files: u64,
    pub source_consolidation_backup_files: u64,
    pub importable_consolidation_backup_files: u64,
    pub source_handoff_files: u64,
    pub importable_handoff_files: u64,
    pub invalid_files: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StickerArchiveAudit {
    pub present: bool,
    pub valid_for_import: Option<bool>,
    pub source_entries: u64,
    pub referenced_assets: u64,
    pub verified_assets: u64,
    pub invalid_entries: u64,
    pub invalid_assets: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct JsonArchiveAudit {
    pub name: String,
    pub present: bool,
    pub valid_for_import: Option<bool>,
    /// Counts only for fixed, known collection names; never includes archive contents.
    pub collection_counts: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DatabaseAudit {
    pub name: String,
    pub present: bool,
    /// `None` means the optional database is absent, so no integrity check ran.
    pub integrity_ok: Option<bool>,
    pub tables: Vec<TableAudit>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TableAudit {
    pub name: String,
    pub columns: Vec<ColumnAudit>,
    pub row_count: i64,
    pub state_counts: BTreeMap<String, i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ColumnAudit {
    pub name: String,
    pub data_type: String,
    pub not_null: bool,
    pub primary_key: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DirectoryAudit {
    pub name: String,
    pub present: bool,
    pub file_count: u64,
    pub total_bytes: u64,
    pub extensions: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ConfigAudit {
    pub present: bool,
    pub leaf_types: BTreeMap<String, String>,
    /// Effective enabled Node features after applying the importer's defaults.
    /// Names only: configuration values and secrets are never included.
    pub enabled_features: Vec<String>,
    /// Enabled Node features not yet behaviorally mapped to the Rust runtime.
    /// Names only; this is the active-startup compatibility gate.
    pub unmapped_enabled_features: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AuditFailure {
    pub source: String,
    pub reason: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct SnapshotReport {
    pub completed: bool,
    pub copied_files: u64,
    pub copied_bytes: u64,
    pub verified_inventory: bool,
    pub audit: MigrationAudit,
}

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("source data directory is missing or unreadable")]
    InvalidSource,
    #[error("snapshot requires an explicit confirmation that the Node writer is stopped")]
    NodeNotStopped,
    #[error("destination must be a new directory outside the source tree")]
    UnsafeDestination,
    #[error("source audit failed; snapshot was not created")]
    SourceAuditFailed,
    #[error("snapshot copy or verification failed; source data was not modified")]
    CopyOrVerifyFailed,
}

pub fn audit_directory(root: impl AsRef<Path>) -> MigrationAudit {
    let root = root.as_ref();
    let mut failures = Vec::new();
    let mut databases = Vec::new();
    for name in discover_databases(root) {
        let path = root.join(&name);
        if !path.is_file() {
            databases.push(DatabaseAudit {
                name,
                present: false,
                integrity_ok: None,
                tables: Vec::new(),
            });
            continue;
        }
        match audit_database(&path, name.clone()) {
            Ok(database) => databases.push(database),
            Err(reason) => {
                failures.push(AuditFailure {
                    source: name.clone(),
                    reason,
                });
                databases.push(DatabaseAudit {
                    name,
                    present: true,
                    integrity_ok: Some(false),
                    tables: Vec::new(),
                });
            }
        }
    }

    let directories = DATA_PATHS
        .iter()
        .map(|(name, paths)| audit_data_bundle(root, name, paths))
        .collect();
    let (memory_archive, memory_archive_invalid) = audit_memory_archive(root);
    if memory_archive_invalid {
        failures.push(AuditFailure {
            source: "memory".into(),
            reason: "invalid_or_unsafe_memory_archive",
        });
    }
    let (session_archive, session_archive_invalid) = audit_session_archive(root);
    if session_archive_invalid {
        failures.push(AuditFailure {
            source: "sessions".into(),
            reason: "invalid_session_archive",
        });
    }
    let (sticker_archive, sticker_archive_invalid) = audit_sticker_archive(root);
    if sticker_archive_invalid {
        failures.push(AuditFailure {
            source: "stickers".into(),
            reason: "invalid_sticker_archive_or_asset",
        });
    }
    let (archives, archive_failures) = audit_json_archives(root);
    failures.extend(archive_failures);
    let config_path = root.join("config.json");
    let config = if config_path.is_file() {
        match fs::read(&config_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        {
            Some(value) => {
                let enabled_features = effective_node_enabled_features(&value);
                let unmapped_enabled_features =
                    crate::migration_import::unmigrated_node_features(&value, &enabled_features);
                Some(ConfigAudit {
                    present: true,
                    leaf_types: collect_json_leaf_types(&value),
                    enabled_features,
                    unmapped_enabled_features,
                })
            }
            None => {
                failures.push(AuditFailure {
                    source: "config.json".into(),
                    reason: "invalid_or_unreadable_json",
                });
                Some(ConfigAudit {
                    present: true,
                    leaf_types: BTreeMap::new(),
                    enabled_features: Vec::new(),
                    unmapped_enabled_features: Vec::new(),
                })
            }
        }
    } else {
        Some(ConfigAudit {
            present: false,
            leaf_types: BTreeMap::new(),
            enabled_features: Vec::new(),
            unmapped_enabled_features: Vec::new(),
        })
    };

    MigrationAudit {
        format_version: 6,
        databases,
        directories,
        memory_archive,
        session_archive,
        sticker_archive,
        archives,
        config,
        failures,
    }
}

/// Pre-parse every JSON file the legacy memory importer may consume. The
/// report is aggregate-only; filenames, identities, and memory text stay out.
fn audit_memory_archive(root: &Path) -> (MemoryArchiveAudit, bool) {
    let backup_root = root.join("memory/backups/consolidation");
    fn visit(path: &Path, backup_root: &Path, report: &mut MemoryArchiveAudit) {
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(_) => {
                report.invalid_files = report.invalid_files.saturating_add(1);
                return;
            }
        };
        for entry in entries {
            let Ok(entry) = entry else {
                report.invalid_files = report.invalid_files.saturating_add(1);
                continue;
            };
            let Ok(kind) = entry.file_type() else {
                report.invalid_files = report.invalid_files.saturating_add(1);
                continue;
            };
            if kind.is_symlink() {
                report.invalid_files = report.invalid_files.saturating_add(1);
                continue;
            }
            if kind.is_dir() {
                visit(&entry.path(), backup_root, report);
                continue;
            }
            if !kind.is_file()
                || !entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "json")
            {
                continue;
            }
            report.source_json_files = report.source_json_files.saturating_add(1);
            let parsed = fs::read(entry.path()).ok().and_then(|bytes| {
                let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
                serde_json::from_slice::<serde_json::Value>(
                    &crate::migration_json::normalize_node_json(bytes),
                )
                .ok()
            });
            let is_handoff = entry.file_name() == "_handoff.json";
            let is_person = entry
                .path()
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == "people");
            let is_consolidation_backup =
                entry
                    .path()
                    .strip_prefix(&backup_root)
                    .ok()
                    .is_some_and(|relative| {
                        relative.components().count() == 2
                            && relative
                                .extension()
                                .is_some_and(|extension| extension == "json")
                    });
            if parsed.is_some() {
                report.parseable_json_files = report.parseable_json_files.saturating_add(1);
            } else if !is_handoff && !is_person && !is_consolidation_backup {
                report.invalid_files = report.invalid_files.saturating_add(1);
            }
            if is_person {
                report.source_person_files = report.source_person_files.saturating_add(1);
                let importable = parsed
                    .as_ref()
                    .is_some_and(crate::migration_import::is_importable_person_memory_record);
                if importable {
                    report.importable_person_files =
                        report.importable_person_files.saturating_add(1);
                } else {
                    report.invalid_files = report.invalid_files.saturating_add(1);
                }
            }
            if is_handoff {
                report.source_handoff_files = report.source_handoff_files.saturating_add(1);
                let valid_chat_key = entry
                    .path()
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|name| name.to_str())
                    .and_then(|name| {
                        let (kind, id) = name.split_once('_')?;
                        (matches!(kind, "group" | "private")
                            && !id.is_empty()
                            && id.len() <= 15
                            && id.bytes().all(|byte| byte.is_ascii_digit()))
                        .then_some(())
                    })
                    .is_some();
                let importable = valid_chat_key
                    && fs::read_to_string(entry.path()).ok().is_some_and(|raw| {
                        crate::application::handoff::ConversationHandoff::from_legacy_json(&raw)
                            .is_ok()
                    });
                if importable {
                    report.importable_handoff_files =
                        report.importable_handoff_files.saturating_add(1);
                } else {
                    report.invalid_files = report.invalid_files.saturating_add(1);
                }
            }
            if is_consolidation_backup {
                report.source_consolidation_backup_files =
                    report.source_consolidation_backup_files.saturating_add(1);
                let importable = parsed.as_ref().is_some_and(
                    crate::migration_import::is_importable_memory_consolidation_backup,
                );
                if importable {
                    report.importable_consolidation_backup_files = report
                        .importable_consolidation_backup_files
                        .saturating_add(1);
                } else {
                    report.invalid_files = report.invalid_files.saturating_add(1);
                }
            }
        }
    }

    let path = root.join("memory");
    let mut report = MemoryArchiveAudit {
        present: false,
        source_json_files: 0,
        parseable_json_files: 0,
        source_person_files: 0,
        importable_person_files: 0,
        source_consolidation_backup_files: 0,
        importable_consolidation_backup_files: 0,
        source_handoff_files: 0,
        importable_handoff_files: 0,
        invalid_files: 0,
    };
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (report, false),
        Err(_) => {
            report.invalid_files = 1;
            return (report, true);
        }
    };
    report.present = true;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        report.invalid_files = 1;
        return (report, true);
    }
    visit(&path, &backup_root, &mut report);
    (report.clone(), report.invalid_files > 0)
}

/// Validate the catalog with the same normalizer as the importer and verify
/// every referenced local asset without exposing entry IDs or file names.
fn audit_sticker_archive(root: &Path) -> (StickerArchiveAudit, bool) {
    let archive = root.join("stickers.json");
    let mut report = StickerArchiveAudit {
        present: false,
        valid_for_import: None,
        source_entries: 0,
        referenced_assets: 0,
        verified_assets: 0,
        invalid_entries: 0,
        invalid_assets: 0,
    };
    let metadata = match fs::symlink_metadata(&archive) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (report, false),
        Err(_) => {
            report.valid_for_import = Some(false);
            report.invalid_entries = 1;
            return (report, true);
        }
    };
    report.present = true;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > crate::stickers::MAX_NODE_STICKER_ARCHIVE_BYTES
    {
        report.valid_for_import = Some(false);
        report.invalid_entries = 1;
        return (report, true);
    }
    let entries = match fs::read(&archive)
        .ok()
        .and_then(|bytes| crate::stickers::parse_node_sticker_archive(&bytes).ok())
    {
        Some(entries) => entries,
        None => {
            report.valid_for_import = Some(false);
            report.invalid_entries = 1;
            return (report, true);
        }
    };
    report.source_entries = entries.len() as u64;
    let mut ids = HashSet::with_capacity(entries.len());
    let mut asset_names = BTreeSet::new();
    for entry in entries {
        if !ids.insert(entry.id) {
            report.invalid_entries = report.invalid_entries.saturating_add(1);
        }
        if let Some(name) = entry.local_file.strip_prefix("sticker-assets/") {
            asset_names.insert(name.to_owned());
        }
    }
    report.referenced_assets = asset_names.len() as u64;
    let assets_root = root.join("sticker-assets");
    let assets_root_valid = fs::symlink_metadata(&assets_root)
        .is_ok_and(|metadata| metadata.file_type().is_dir() && !metadata.file_type().is_symlink());
    for name in asset_names {
        let valid = assets_root_valid
            && fs::symlink_metadata(assets_root.join(name)).is_ok_and(|metadata| {
                metadata.file_type().is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.len() <= 8 * 1024 * 1024
            });
        if valid {
            report.verified_assets = report.verified_assets.saturating_add(1);
        } else {
            report.invalid_assets = report.invalid_assets.saturating_add(1);
        }
    }
    let invalid = report.invalid_entries > 0 || report.invalid_assets > 0;
    report.valid_for_import = Some(!invalid);
    (report, invalid)
}

/// Validate precisely the direct JSON entries consumed by the legacy-session
/// importer. Keep filenames and record contents out of the audit report.
fn audit_session_archive(root: &Path) -> (SessionArchiveAudit, bool) {
    let path = root.join("sessions");
    let mut report = SessionArchiveAudit {
        present: false,
        source_json_files: 0,
        importable_json_files: 0,
        invalid_json_files: 0,
    };
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (report, false),
        Err(_) => {
            report.invalid_json_files = 1;
            return (report, true);
        }
    };
    report.present = true;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        report.invalid_json_files = 1;
        return (report, true);
    }
    let entries = match fs::read_dir(&path) {
        Ok(entries) => entries,
        Err(_) => {
            report.invalid_json_files = 1;
            return (report, true);
        }
    };
    for entry in entries {
        let Ok(entry) = entry else {
            report.invalid_json_files = report.invalid_json_files.saturating_add(1);
            continue;
        };
        let entry_path = entry.path();
        if !entry_path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            continue;
        }
        report.source_json_files = report.source_json_files.saturating_add(1);
        let valid = (|| -> Option<()> {
            let file_type = entry.file_type().ok()?;
            if !file_type.is_file() || file_type.is_symlink() {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            i64::try_from(metadata.len()).ok()?;
            let bytes = fs::read(&entry_path).ok()?;
            let normalized = crate::migration_json::normalize_node_json(&bytes);
            let value: serde_json::Value = serde_json::from_slice(&normalized).ok()?;
            if !value.is_object() {
                return None;
            }
            let session_id = entry_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.is_empty())?;
            if value
                .get("id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|id| id != session_id)
            {
                return None;
            }
            Some(())
        })()
        .is_some();
        if valid {
            report.importable_json_files = report.importable_json_files.saturating_add(1);
        } else {
            report.invalid_json_files = report.invalid_json_files.saturating_add(1);
        }
    }
    (report.clone(), report.invalid_json_files > 0)
}

fn audit_json_archives(root: &Path) -> (Vec<JsonArchiveAudit>, Vec<AuditFailure>) {
    let specs: &[(&str, &[&str])] = &[
        ("daily-moments.json", &["records", "scheduleSlots"]),
        (
            "qzone-interactions.json",
            &["feeds", "comments", "watchedPosts", "runs"],
        ),
    ];
    let mut reports = Vec::with_capacity(specs.len());
    let mut failures = Vec::new();
    for (name, collections) in specs {
        let path = root.join(name);
        let report = match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => JsonArchiveAudit {
                name: (*name).to_owned(),
                present: false,
                valid_for_import: None,
                collection_counts: BTreeMap::new(),
            },
            Err(_) => {
                failures.push(AuditFailure {
                    source: (*name).to_owned(),
                    reason: "unreadable_archive_metadata",
                });
                JsonArchiveAudit {
                    name: (*name).to_owned(),
                    present: true,
                    valid_for_import: None,
                    collection_counts: BTreeMap::new(),
                }
            }
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                failures.push(AuditFailure {
                    source: (*name).to_owned(),
                    reason: "unsafe_archive_file_type",
                });
                JsonArchiveAudit {
                    name: (*name).to_owned(),
                    present: true,
                    valid_for_import: None,
                    collection_counts: BTreeMap::new(),
                }
            }
            Ok(metadata)
                if metadata.len()
                    > match *name {
                        "daily-moments.json" => {
                            crate::daily_moments::MAX_DAILY_MOMENTS_ARCHIVE_BYTES
                        }
                        _ => crate::qzone_interactions::MAX_QZONE_INTERACTIONS_ARCHIVE_BYTES,
                    } =>
            {
                failures.push(AuditFailure {
                    source: (*name).to_owned(),
                    reason: "archive_exceeds_import_size_limit",
                });
                JsonArchiveAudit {
                    name: (*name).to_owned(),
                    present: true,
                    valid_for_import: Some(false),
                    collection_counts: BTreeMap::new(),
                }
            }
            Ok(_) => match fs::read(&path).ok().and_then(|bytes| {
                serde_json::from_slice::<serde_json::Value>(&bytes)
                    .ok()
                    .map(|value| (value, bytes))
            }) {
                Some((value, bytes)) if value.is_object() => {
                    let import_valid = match *name {
                        "daily-moments.json" => {
                            crate::daily_moments::validate_node_archive_bytes(&bytes).is_ok()
                        }
                        _ => crate::qzone_interactions::validate_node_archive_bytes(&bytes).is_ok(),
                    };
                    let mut collection_counts = BTreeMap::new();
                    let mut malformed = !import_valid;
                    for collection in *collections {
                        match value.get(*collection) {
                            None => {
                                collection_counts.insert((*collection).to_owned(), 0);
                            }
                            Some(serde_json::Value::Array(items)) => {
                                collection_counts
                                    .insert((*collection).to_owned(), items.len() as u64);
                            }
                            Some(_) => malformed = true,
                        }
                    }
                    if malformed {
                        failures.push(AuditFailure {
                            source: (*name).to_owned(),
                            reason: if import_valid {
                                "invalid_archive_collection"
                            } else {
                                "archive_rejected_by_importer"
                            },
                        });
                    }
                    JsonArchiveAudit {
                        name: (*name).to_owned(),
                        present: true,
                        valid_for_import: Some(!malformed),
                        collection_counts,
                    }
                }
                _ => {
                    failures.push(AuditFailure {
                        source: (*name).to_owned(),
                        reason: "invalid_or_unreadable_archive_json",
                    });
                    JsonArchiveAudit {
                        name: (*name).to_owned(),
                        present: true,
                        valid_for_import: Some(false),
                        collection_counts: BTreeMap::new(),
                    }
                }
            },
        };
        reports.push(report);
    }
    (reports, failures)
}

/// Mirrors Node's merged defaults and forced-stable-feature policy. Shared by
/// the read-only preflight audit and the stopped-writer importer so the feature
/// inventory cannot silently diverge between those gates.
pub fn effective_node_enabled_features(config: &serde_json::Value) -> Vec<String> {
    let mut enabled = Vec::new();
    for (name, path, default_enabled, forced_enabled) in [
        ("visionInput", &["api", "vision"][..], true, false),
        (
            "visionFallback",
            &["api", "visionModel", "enabled"][..],
            false,
            false,
        ),
        ("webSearch", &["webSearch", "enabled"][..], true, false),
        ("proactive", &["proactive", "enabled"][..], false, false),
        (
            "followUp",
            &["proactive", "followUpEnabled"][..],
            true,
            false,
        ),
        (
            "selfWake",
            &["proactive", "selfWakeEnabled"][..],
            true,
            false,
        ),
        // These behavior-changing switches are not part of the older named
        // pilot list. Inventory them so enabled Node-only semantics cannot
        // bypass the importer gate.
        ("pacing", &["pacing", "enabled"][..], false, false),
        (
            "toolSchedulerPilot",
            &["toolSchedulerPilot", "enabled"][..],
            false,
            false,
        ),
        ("groupInvite", &["groupInvite", "enabled"][..], false, false),
        (
            "dailyMoments",
            &["dailyMoments", "enabled"][..],
            false,
            false,
        ),
        (
            "qzoneInteractions",
            &["qzoneInteractions", "enabled"][..],
            false,
            false,
        ),
        (
            "identityPilot",
            &["identityPilot", "enabled"][..],
            false,
            true,
        ),
        ("slangPilot", &["slangPilot", "enabled"][..], false, false),
        (
            "incomingFriendRequest",
            &["identityPilot", "incomingFriendRequest", "enabled"][..],
            false,
            false,
        ),
        (
            "incidentPilot",
            &["incidentPilot", "enabled"][..],
            false,
            true,
        ),
        (
            "relationshipPilot",
            &["relationshipPilot", "enabled"][..],
            false,
            false,
        ),
        ("sticker", &["sticker", "enabled"][..], true, false),
        (
            "stickerCollect",
            &["sticker", "collectEnabled"][..],
            true,
            false,
        ),
        (
            "memoryConsolidate",
            &["memory", "consolidateEnabled"][..],
            true,
            false,
        ),
        (
            "memoryHandoff",
            &["memory", "handoffEnabled"][..],
            true,
            false,
        ),
        (
            "multimodalContext",
            &["multimodalContextPilot", "enabled"][..],
            false,
            false,
        ),
    ] {
        let value = path
            .iter()
            .try_fold(config, |current, key| current.get(*key))
            .and_then(serde_json::Value::as_bool);
        if forced_enabled || value.unwrap_or(default_enabled) {
            enabled.push(name.to_owned());
        }
    }
    let fallback = config.get("api").and_then(|api| api.get("fallback"));
    let fallback_model = fallback
        .and_then(|settings| settings.get("model"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if !fallback_model.trim().is_empty()
        && fallback
            .and_then(|settings| settings.get("enabled"))
            .and_then(serde_json::Value::as_bool)
            != Some(false)
    {
        enabled.push("modelFallback".to_owned());
    }
    // RecallNotifier is an always-on Node behavior when the owner target is
    // configured, rather than a named feature flag. Include it in the cutover
    // inventory so an implicit write/notification path cannot disappear.
    let recall_owner_configured = config
        .get("admin")
        .and_then(|admin| admin.get("ownerUin"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|owner| {
            let owner = owner.trim();
            (5..=15).contains(&owner.len()) && owner.bytes().all(|byte| byte.is_ascii_digit())
        });
    if recall_owner_configured {
        enabled.push("recallNotifications".to_owned());
    }
    let conversation = config
        .get("conversation")
        .and_then(serde_json::Value::as_object);
    let mode_is_threaded = |value: Option<&serde_json::Value>| {
        matches!(
            value.and_then(serde_json::Value::as_str),
            Some("threaded" | "lifecycle")
        )
    };
    let conversation_enabled = conversation.is_some_and(|conversation| {
        mode_is_threaded(conversation.get("mode"))
            || (conversation
                .get("unifiedMode")
                .and_then(serde_json::Value::as_bool)
                == Some(false)
                && conversation
                    .get("groupModes")
                    .and_then(serde_json::Value::as_object)
                    .is_some_and(|groups| groups.values().any(|mode| mode_is_threaded(Some(mode)))))
    });
    if conversation_enabled {
        enabled.push("conversationThreads".to_owned());
    }
    let asr_enabled = config
        .get("asr")
        .and_then(|value| value.get("enabled"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let asr_provider = config
        .get("asr")
        .and_then(|value| value.get("provider"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("openai")
        .trim();
    if asr_enabled && !asr_provider.eq_ignore_ascii_case("openai") {
        enabled.push("asr".to_owned());
    }
    enabled
}

fn discover_databases(root: &Path) -> Vec<String> {
    let mut names = KNOWN_DATABASES
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().is_ok_and(|kind| kind.is_file())
                && (name.ends_with(".sqlite") || name.ends_with(".db"))
                && !names.contains(&name)
            {
                names.push(name);
            }
        }
    }
    names.sort();
    names
}

fn audit_database(path: &Path, name: String) -> Result<DatabaseAudit, &'static str> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "sqlite_open_failed")?;
    connection
        .execute_batch("PRAGMA query_only = ON; BEGIN DEFERRED;")
        .map_err(|_| "sqlite_read_transaction_failed")?;
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|_| "sqlite_integrity_check_failed")?;
    if integrity != "ok" {
        return Err("sqlite_integrity_check_failed");
    }

    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .map_err(|_| "sqlite_schema_read_failed")?;
    let table_names = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|_| "sqlite_schema_read_failed")?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "sqlite_schema_read_failed")?;
    drop(statement);

    let mut tables = Vec::with_capacity(table_names.len());
    for table in table_names {
        let escaped = table.replace('"', "\"\"");
        let quoted = format!("\"{escaped}\"");
        let row_count = connection
            .query_row(&format!("SELECT COUNT(*) FROM {quoted}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|_| "sqlite_table_count_failed")?;
        let mut columns_statement = connection
            .prepare(&format!("PRAGMA table_info({quoted})"))
            .map_err(|_| "sqlite_schema_read_failed")?;
        let columns = columns_statement
            .query_map([], |row| {
                Ok(ColumnAudit {
                    name: row.get(1)?,
                    data_type: row.get::<_, String>(2)?,
                    not_null: row.get::<_, i64>(3)? != 0,
                    primary_key: row.get::<_, i64>(5)? != 0,
                })
            })
            .map_err(|_| "sqlite_schema_read_failed")?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "sqlite_schema_read_failed")?;

        let mut state_counts = BTreeMap::new();
        if columns
            .iter()
            .any(|column| column.name == "state" || column.name == "status")
        {
            let state_column = if columns.iter().any(|column| column.name == "state") {
                "state"
            } else {
                "status"
            };
            let state_identifier = format!("\"{state_column}\"");
            let mut states = connection
                .prepare(&format!(
                    "SELECT {state_identifier}, COUNT(*) FROM {quoted} GROUP BY {state_identifier}"
                ))
                .map_err(|_| "sqlite_state_count_failed")?;
            let rows = states
                .query_map([], |row| {
                    Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(|_| "sqlite_state_count_failed")?;
            for row in rows {
                let (value, count) = row.map_err(|_| "sqlite_state_count_failed")?;
                let label = value
                    .as_deref()
                    .filter(|value| STATE_VALUES.contains(value))
                    .unwrap_or("other")
                    .to_owned();
                *state_counts.entry(label).or_insert(0) += count;
            }
        }
        tables.push(TableAudit {
            name: table,
            columns,
            row_count,
            state_counts,
        });
    }
    connection
        .execute_batch("ROLLBACK;")
        .map_err(|_| "sqlite_read_transaction_failed")?;
    Ok(DatabaseAudit {
        name,
        present: true,
        integrity_ok: Some(true),
        tables,
    })
}

/// Copy an already-quiesced legacy data directory into a new destination.
/// The caller must explicitly assert the Node writer is stopped. Source files
/// are never modified; the staged copy is renamed into place only after an
/// inventory and byte-for-byte verification pass.
pub fn snapshot_directory(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
    node_stopped: bool,
) -> Result<SnapshotReport, SnapshotError> {
    if !node_stopped {
        return Err(SnapshotError::NodeNotStopped);
    }
    let source = source
        .as_ref()
        .canonicalize()
        .map_err(|_| SnapshotError::InvalidSource)?;
    if !source.is_dir() {
        return Err(SnapshotError::InvalidSource);
    }
    let destination_arg = destination.as_ref();
    if destination_arg.exists() {
        return Err(SnapshotError::UnsafeDestination);
    }
    let destination_name = destination_arg
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or(SnapshotError::UnsafeDestination)?;
    let parent = destination_arg
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()
        .map_err(|_| SnapshotError::UnsafeDestination)?;
    let destination = parent.join(destination_name);
    if destination == source || destination.starts_with(&source) {
        return Err(SnapshotError::UnsafeDestination);
    }

    let source_audit = audit_directory(&source);
    if !source_audit.failures.is_empty() {
        return Err(SnapshotError::SourceAuditFailed);
    }
    let staging = parent.join(format!(
        ".{}.rinbot-staging-{}",
        destination_name.to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    fs::create_dir(&staging).map_err(|_| SnapshotError::CopyOrVerifyFailed)?;
    if set_directory_private(&staging).is_err() {
        let _ = fs::remove_dir_all(&staging);
        return Err(SnapshotError::CopyOrVerifyFailed);
    }

    let result = (|| {
        let mut stats = CopyStats::default();
        copy_tree(&source, &staging, &mut stats)?;
        let copied_audit = audit_directory(&staging);
        if !copied_audit.failures.is_empty() || copied_audit != source_audit {
            return Err(SnapshotError::CopyOrVerifyFailed);
        }
        let (verified_files, verified_bytes) = verify_tree_contents(&source, &staging)?;
        if verified_files != stats.files || verified_bytes != stats.bytes {
            return Err(SnapshotError::CopyOrVerifyFailed);
        }
        if destination.exists() {
            return Err(SnapshotError::UnsafeDestination);
        }
        fs::rename(&staging, &destination).map_err(|_| SnapshotError::CopyOrVerifyFailed)?;
        Ok(SnapshotReport {
            completed: true,
            copied_files: stats.files,
            copied_bytes: stats.bytes,
            verified_inventory: true,
            audit: copied_audit,
        })
    })();
    if result.is_err() && staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

#[derive(Default)]
struct CopyStats {
    files: u64,
    bytes: u64,
}

fn copy_tree(
    source: &Path,
    destination: &Path,
    stats: &mut CopyStats,
) -> Result<(), SnapshotError> {
    for entry in fs::read_dir(source).map_err(|_| SnapshotError::CopyOrVerifyFailed)? {
        let entry = entry.map_err(|_| SnapshotError::CopyOrVerifyFailed)?;
        let kind = entry
            .file_type()
            .map_err(|_| SnapshotError::CopyOrVerifyFailed)?;
        let output = destination.join(entry.file_name());
        if kind.is_symlink() {
            return Err(SnapshotError::CopyOrVerifyFailed);
        }
        if kind.is_dir() {
            fs::create_dir(&output).map_err(|_| SnapshotError::CopyOrVerifyFailed)?;
            set_directory_private(&output).map_err(|_| SnapshotError::CopyOrVerifyFailed)?;
            copy_tree(&entry.path(), &output, stats)?;
        } else if kind.is_file() {
            let mut input =
                fs::File::open(entry.path()).map_err(|_| SnapshotError::CopyOrVerifyFailed)?;
            let mut output_file = private_file(&output)?;
            stats.bytes += std::io::copy(&mut input, &mut output_file)
                .map_err(|_| SnapshotError::CopyOrVerifyFailed)?;
            output_file
                .flush()
                .map_err(|_| SnapshotError::CopyOrVerifyFailed)?;
            stats.files += 1;
        } else {
            return Err(SnapshotError::CopyOrVerifyFailed);
        }
    }
    Ok(())
}

fn verify_tree_contents(source: &Path, destination: &Path) -> Result<(u64, u64), SnapshotError> {
    let mut files = 0;
    let mut bytes = 0;
    for entry in fs::read_dir(source).map_err(|_| SnapshotError::CopyOrVerifyFailed)? {
        let entry = entry.map_err(|_| SnapshotError::CopyOrVerifyFailed)?;
        let kind = entry
            .file_type()
            .map_err(|_| SnapshotError::CopyOrVerifyFailed)?;
        let copied = destination.join(entry.file_name());
        if kind.is_dir() {
            let (nested_files, nested_bytes) = verify_tree_contents(&entry.path(), &copied)?;
            files += nested_files;
            bytes += nested_bytes;
        } else if kind.is_file() {
            if !files_equal(&entry.path(), &copied)? {
                return Err(SnapshotError::CopyOrVerifyFailed);
            }
            files += 1;
            bytes += entry
                .metadata()
                .map_err(|_| SnapshotError::CopyOrVerifyFailed)?
                .len();
        } else {
            return Err(SnapshotError::CopyOrVerifyFailed);
        }
    }
    Ok((files, bytes))
}

fn files_equal(left: &Path, right: &Path) -> Result<bool, SnapshotError> {
    let mut left_file = fs::File::open(left).map_err(|_| SnapshotError::CopyOrVerifyFailed)?;
    let mut right_file = fs::File::open(right).map_err(|_| SnapshotError::CopyOrVerifyFailed)?;
    let mut left_buffer = [0_u8; 64 * 1024];
    let mut right_buffer = [0_u8; 64 * 1024];
    loop {
        let left_len = left_file
            .read(&mut left_buffer)
            .map_err(|_| SnapshotError::CopyOrVerifyFailed)?;
        let right_len = right_file
            .read(&mut right_buffer)
            .map_err(|_| SnapshotError::CopyOrVerifyFailed)?;
        if left_len != right_len || left_buffer[..left_len] != right_buffer[..right_len] {
            return Ok(false);
        }
        if left_len == 0 {
            return Ok(true);
        }
    }
}

fn set_directory_private(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn private_file(path: &Path) -> Result<fs::File, SnapshotError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|_| SnapshotError::CopyOrVerifyFailed)
}

fn audit_data_bundle(root: &Path, name: &str, paths: &[&str]) -> DirectoryAudit {
    let mut report = DirectoryAudit {
        name: name.to_owned(),
        present: false,
        file_count: 0,
        total_bytes: 0,
        extensions: BTreeMap::new(),
    };
    for relative in paths {
        let path = root.join(relative);
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        let kind = metadata.file_type();
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            report.present = true;
            collect_directory(&path, &mut report);
        } else if kind.is_file() {
            report.present = true;
            record_file(&path, metadata.len(), &mut report);
        }
    }
    report
}

fn collect_directory(path: &Path, report: &mut DirectoryAudit) {
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            collect_directory(&entry.path(), report);
        } else if kind.is_file() {
            record_file(
                &entry.path(),
                entry.metadata().map(|metadata| metadata.len()).unwrap_or(0),
                report,
            );
        }
    }
}

fn record_file(path: &Path, bytes: u64, report: &mut DirectoryAudit) {
    report.file_count += 1;
    report.total_bytes = report.total_bytes.saturating_add(bytes);
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|value| value.len() <= 12)
        .unwrap_or_else(|| "[none]".to_owned());
    *report.extensions.entry(extension).or_insert(0) += 1;
}

fn collect_json_leaf_types(value: &serde_json::Value) -> BTreeMap<String, String> {
    fn visit(value: &serde_json::Value, prefix: &str, output: &mut BTreeMap<String, String>) {
        match value {
            serde_json::Value::Object(object) => {
                for (key, nested) in object {
                    let key = if redact_dynamic_config_key(prefix, key) {
                        "[redacted]"
                    } else {
                        key
                    };
                    let path = if prefix.is_empty() {
                        key.to_owned()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    visit(nested, &path, output);
                }
            }
            serde_json::Value::Array(array) => {
                if array.is_empty() {
                    output.insert(format!("{prefix}[]"), "empty_array".into());
                } else {
                    visit(&array[0], &format!("{prefix}[]"), output);
                }
            }
            serde_json::Value::Null => {
                output.insert(prefix.into(), "null".into());
            }
            serde_json::Value::Bool(_) => {
                output.insert(prefix.into(), "boolean".into());
            }
            serde_json::Value::Number(_) => {
                output.insert(prefix.into(), "number".into());
            }
            serde_json::Value::String(_) => {
                output.insert(prefix.into(), "string".into());
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(value, "", &mut result);
    result
}

fn redact_dynamic_config_key(parent_path: &str, key: &str) -> bool {
    let numeric_identifier =
        (5..=15).contains(&key.len()) && key.bytes().all(|byte| byte.is_ascii_digit());
    let dynamic_map = matches!(
        parent_path,
        "providerKeys" | "timeControl.overrides" | "conversation.groupModes"
    ) || parent_path.ends_with(".groupSliderPos")
        || parent_path.ends_with(".groupModes")
        || parent_path.ends_with(".modelNames");
    numeric_identifier || dynamic_map
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    #[test]
    fn audit_counts_legacy_root_sticker_catalog_and_assets_without_following_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("sticker-assets")).unwrap();
        fs::create_dir_all(directory.path().join("stickers/nested")).unwrap();
        fs::write(
            directory.path().join("stickers.json"),
            r#"[{"id":"entry","localFile":"sticker-assets/one.png"}]"#,
        )
        .unwrap();
        fs::write(
            directory.path().join("sticker-assets/one.png"),
            [1, 2, 3, 4],
        )
        .unwrap();
        fs::write(directory.path().join("stickers/nested/two.webp"), [5, 6, 7]).unwrap();
        #[cfg(unix)]
        {
            let outside = directory.path().join("outside-secret.jpg");
            fs::write(&outside, [8, 9, 10, 11, 12]).unwrap();
            std::os::unix::fs::symlink(
                outside,
                directory.path().join("sticker-assets/external.jpg"),
            )
            .unwrap();
        }

        let report = audit_directory(directory.path());
        let stickers = report
            .directories
            .iter()
            .find(|entry| entry.name == "stickers")
            .unwrap();
        assert!(stickers.present);
        assert_eq!(stickers.file_count, 3);
        assert_eq!(
            stickers.total_bytes,
            fs::metadata(directory.path().join("stickers.json"))
                .unwrap()
                .len()
                + 4
                + 3
        );
        assert_eq!(stickers.extensions.get("json"), Some(&1));
        assert_eq!(stickers.extensions.get("png"), Some(&1));
        assert_eq!(stickers.extensions.get("webp"), Some(&1));
        assert_eq!(report.sticker_archive.valid_for_import, Some(true));
        assert_eq!(report.sticker_archive.source_entries, 1);
        assert_eq!(report.sticker_archive.referenced_assets, 1);
        assert_eq!(report.sticker_archive.verified_assets, 1);
        assert_eq!(report.sticker_archive.invalid_assets, 0);
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains("sticker-assets/one.png"));
        assert!(!serialized.contains("outside-secret"));
    }

    #[test]
    fn sticker_archive_audit_rejects_duplicate_ids_and_missing_assets_privately() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("stickers.json"),
            r#"[{"id":"private-id","localFile":"sticker-assets/missing.png"},{"id":"private-id"}]"#,
        )
        .unwrap();

        let report = audit_directory(directory.path());
        assert_eq!(report.sticker_archive.valid_for_import, Some(false));
        assert_eq!(report.sticker_archive.source_entries, 2);
        assert_eq!(report.sticker_archive.invalid_entries, 1);
        assert_eq!(report.sticker_archive.referenced_assets, 1);
        assert_eq!(report.sticker_archive.invalid_assets, 1);
        assert!(report.failures.iter().any(|failure| {
            failure.source == "stickers" && failure.reason == "invalid_sticker_archive_or_asset"
        }));
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains("private-id"));
        assert!(!serialized.contains("missing.png"));
    }

    #[test]
    fn audit_is_read_only_aggregated_and_redacts_values() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("messages.sqlite");
        let connection = Connection::open(&database_path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE messages(id INTEGER PRIMARY KEY, text TEXT, state TEXT);
                 CREATE TABLE outbox(id TEXT PRIMARY KEY, payload TEXT, state TEXT);",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO messages(text,state) VALUES (?1,?2)",
                params!["private content must not leak", "pending"],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO outbox(id,payload,state) VALUES (?1,?2,?3)",
                params!["qq-123456789", "secret payload", "unknown"],
            )
            .unwrap();
        drop(connection);
        fs::write(
            directory.path().join("config.json"),
            r#"{"enabled":true,"token":"do-not-report-this","providerKeys":{"private-provider-id":"private-key"},"store":{"groupSliderPos":{"123456789":7}},"dailyMoments":{"enabled":true},"webSearch":{"enabled":false},"sticker":{"enabled":false}}"#,
        )
        .unwrap();
        let config_before = fs::read(directory.path().join("config.json")).unwrap();
        let daily_archive = directory.path().join("daily-moments.json");
        fs::write(
            &daily_archive,
            r#"{"version":1,"records":[{"id":"moment-1","dayKey":"2026-10-01","status":"published","content":"private daily text must not leak"},{"id":"moment-2","dayKey":"2026-10-02","status":"draft"}],"scheduleSlots":[{"id":"slot-1","dayKey":"2026-10-02","status":"pending"}]}"#,
        )
        .unwrap();
        let daily_archive_before = fs::read(&daily_archive).unwrap();
        let qzone_archive = directory.path().join("qzone-interactions.json");
        fs::write(
            &qzone_archive,
            r#"{"version":1,"feeds":[{"key":"feed-1","content":"private feed text must not leak"}],"comments":[],"watchedPosts":[{"key":"watch-1"}],"runs":[{"id":"run-1"},{"id":"run-2"}]}"#,
        )
        .unwrap();
        let qzone_archive_before = fs::read(&qzone_archive).unwrap();
        fs::create_dir(directory.path().join("sessions")).unwrap();
        fs::write(directory.path().join("sessions/private-123456.json"), "{}").unwrap();
        let session_path = directory.path().join("sessions/private-123456.json");
        let session_before = fs::read(&session_path).unwrap();

        let report = audit_directory(directory.path());
        assert_eq!(report.format_version, 6);
        assert_eq!(report.session_archive.source_json_files, 1);
        assert_eq!(report.session_archive.importable_json_files, 1);
        assert_eq!(report.session_archive.invalid_json_files, 0);
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"row_count\":1"));
        assert!(json.contains("\"pending\":1"));
        assert!(json.contains("\"unknown\":1"));
        assert!(json.contains("\"sessions\""));
        let daily = report
            .archives
            .iter()
            .find(|archive| archive.name == "daily-moments.json")
            .unwrap();
        assert_eq!(daily.valid_for_import, Some(true));
        assert_eq!(daily.collection_counts["records"], 2);
        assert_eq!(daily.collection_counts["scheduleSlots"], 1);
        let qzone = report
            .archives
            .iter()
            .find(|archive| archive.name == "qzone-interactions.json")
            .unwrap();
        assert_eq!(qzone.valid_for_import, Some(true));
        assert_eq!(qzone.collection_counts["feeds"], 1);
        assert_eq!(qzone.collection_counts["comments"], 0);
        assert_eq!(qzone.collection_counts["watchedPosts"], 1);
        assert_eq!(qzone.collection_counts["runs"], 2);
        assert!(json.contains("\"token\":\"string\""));
        let enabled = &report.config.as_ref().unwrap().enabled_features;
        assert!(enabled.contains(&"dailyMoments".to_owned()));
        assert!(enabled.contains(&"identityPilot".to_owned()));
        assert!(enabled.contains(&"incidentPilot".to_owned()));
        assert!(enabled.contains(&"visionInput".to_owned()));
        assert!(!enabled.contains(&"webSearch".to_owned()));
        assert!(!enabled.contains(&"sticker".to_owned()));
        let unmapped = &report.config.as_ref().unwrap().unmapped_enabled_features;
        assert!(!unmapped.contains(&"dailyMoments".to_owned()));
        assert!(!unmapped.contains(&"identityPilot".to_owned()));
        assert!(!unmapped.contains(&"incidentPilot".to_owned()));
        assert!(unmapped.contains(&"visionInput".to_owned()));
        assert!(!unmapped.contains(&"webSearch".to_owned()));
        assert!(!json.contains("private content must not leak"));
        assert!(!json.contains("secret payload"));
        assert!(!json.contains("private daily text must not leak"));
        assert!(!json.contains("private feed text must not leak"));
        assert!(!json.contains("do-not-report-this"));
        assert!(!json.contains("123456789"));
        assert!(!json.contains("private-provider-id"));
        assert!(json.contains("[redacted]"));
        assert!(!json.contains("123456"));
        assert!(
            database_path.exists(),
            "audit must not replace or remove source files"
        );
        assert_eq!(
            config_before,
            fs::read(directory.path().join("config.json")).unwrap()
        );
        assert_eq!(daily_archive_before, fs::read(&daily_archive).unwrap());
        assert_eq!(qzone_archive_before, fs::read(&qzone_archive).unwrap());
        assert_eq!(session_before, fs::read(&session_path).unwrap());
        let check = Connection::open_with_flags(
            &database_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap();
        let persisted_rows: i64 = check
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(persisted_rows, 1);
    }

    #[test]
    fn session_audit_prevalidates_import_rules_without_disclosing_archive_content() {
        let directory = tempfile::tempdir().unwrap();
        let sessions = directory.path().join("sessions");
        fs::create_dir(&sessions).unwrap();
        fs::write(
            sessions.join("good.json"),
            br#"{"id":"good","summary":"private \\uD800 note"}"#,
        )
        .unwrap();
        fs::write(
            sessions.join("mismatch-private-id.json"),
            br#"{"id":"different","summary":"private mismatch payload"}"#,
        )
        .unwrap();
        fs::write(
            sessions.join("invalid-private-id.json"),
            br#"{"summary":"private invalid payload""#,
        )
        .unwrap();

        let report = audit_directory(directory.path());
        assert!(report.session_archive.present);
        assert_eq!(report.session_archive.source_json_files, 3);
        assert_eq!(report.session_archive.importable_json_files, 1);
        assert_eq!(report.session_archive.invalid_json_files, 2);
        assert!(report.failures.iter().any(|failure| {
            failure.source == "sessions" && failure.reason == "invalid_session_archive"
        }));
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains("mismatch-private-id"));
        assert!(!serialized.contains("invalid-private-id"));
        assert!(!serialized.contains("private mismatch payload"));
        assert!(!serialized.contains("private invalid payload"));
    }

    #[test]
    fn audit_rejects_malformed_active_feature_archives_without_echoing_contents() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("daily-moments.json"),
            r#"{"records":"private malformed payload"}"#,
        )
        .unwrap();
        fs::write(
            directory.path().join("qzone-interactions.json"),
            "not-json-private-payload",
        )
        .unwrap();

        let report = audit_directory(directory.path());
        assert_eq!(report.format_version, 6);
        for name in ["daily-moments.json", "qzone-interactions.json"] {
            assert_eq!(
                report
                    .archives
                    .iter()
                    .find(|archive| archive.name == name)
                    .unwrap()
                    .valid_for_import,
                Some(false)
            );
        }
        let serialized = serde_json::to_string(&report).unwrap();
        assert_eq!(report.failures.len(), 2);
        assert!(!serialized.contains("private malformed payload"));
        assert!(!serialized.contains("not-json-private-payload"));
    }

    #[test]
    fn audit_rejects_daily_and_qzone_rows_the_importers_would_refuse() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("daily-moments.json"),
            r#"{"version":1,"records":[{"id":"private-daily-id"}],"scheduleSlots":[]}"#,
        )
        .unwrap();
        fs::write(
            directory.path().join("qzone-interactions.json"),
            r#"{"version":1,"feeds":[{"content":"private-qzone-content"}],"comments":[],"watchedPosts":[],"runs":[]}"#,
        )
        .unwrap();

        let report = audit_directory(directory.path());
        for name in ["daily-moments.json", "qzone-interactions.json"] {
            assert_eq!(
                report
                    .archives
                    .iter()
                    .find(|archive| archive.name == name)
                    .unwrap()
                    .valid_for_import,
                Some(false)
            );
            assert!(report.failures.iter().any(|failure| failure.source == name));
        }
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains("private-daily-id"));
        assert!(!serialized.contains("private-qzone-content"));
    }

    #[test]
    fn memory_archive_audit_counts_json_parseability_without_disclosing_content() {
        let directory = tempfile::tempdir().unwrap();
        let memory = directory.path().join("memory");
        fs::create_dir_all(memory.join("people")).unwrap();
        fs::create_dir_all(memory.join("group_123")).unwrap();
        fs::create_dir_all(memory.join("private_456")).unwrap();
        fs::create_dir_all(memory.join("backups/consolidation/123")).unwrap();
        fs::create_dir_all(memory.join("backups/consolidation/456")).unwrap();
        fs::write(
            memory.join("people/private-person-id.json"),
            b"\xef\xbb\xbf{\"note\":\"private memory text\"}",
        )
        .unwrap();
        fs::write(
            memory.join("people/123.json"),
            br#"{"userId":"123","impressions":["safe-shaped record"]}"#,
        )
        .unwrap();
        fs::write(
            memory.join("group_123/_handoff.json"),
            b"{invalid private memory payload",
        )
        .unwrap();
        fs::write(
            memory.join("private_456/_handoff.json"),
            br#"{"version":1,"topic":"private handoff"}"#,
        )
        .unwrap();
        fs::write(
            memory.join("backups/consolidation/123/valid.json"),
            br#"{"version":1,"reason":"manual","backedUpAt":1,"person":{"userId":"123","impressions":["private backup text"]}}"#,
        )
        .unwrap();
        fs::write(
            memory.join("backups/consolidation/456/invalid.json"),
            br#"{"version":1,"reason":"","backedUpAt":0,"person":{"impressions":[]}}"#,
        )
        .unwrap();

        let report = audit_directory(directory.path());
        assert!(report.memory_archive.present);
        assert_eq!(report.memory_archive.source_json_files, 6);
        assert_eq!(report.memory_archive.parseable_json_files, 5);
        assert_eq!(report.memory_archive.source_person_files, 2);
        assert_eq!(report.memory_archive.importable_person_files, 1);
        assert_eq!(report.memory_archive.source_consolidation_backup_files, 2);
        assert_eq!(
            report.memory_archive.importable_consolidation_backup_files,
            1
        );
        assert_eq!(report.memory_archive.source_handoff_files, 2);
        assert_eq!(report.memory_archive.importable_handoff_files, 1);
        assert_eq!(report.memory_archive.invalid_files, 3);
        assert!(report
            .failures
            .iter()
            .any(|failure| failure.source == "memory"));
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains("private-person-id"));
        assert!(!serialized.contains("private_456"));
        assert!(!serialized.contains("private memory text"));
        assert!(!serialized.contains("private backup text"));
        assert!(!serialized.contains("invalid private memory payload"));
    }

    #[test]
    fn snapshot_requires_stopped_writer_and_verifies_private_copy() {
        let parent = tempfile::tempdir().unwrap();
        let source = parent.path().join("node-data");
        let destination = parent.path().join("rinbot-snapshot");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("config.json"), r#"{"safe":true}"#).unwrap();
        fs::create_dir(source.join("sessions")).unwrap();
        fs::write(source.join("sessions/session.json"), r#"{"items":[1,2]}"#).unwrap();
        let database = Connection::open(source.join("messages.sqlite")).unwrap();
        database
            .execute_batch("CREATE TABLE messages(id INTEGER PRIMARY KEY, state TEXT); INSERT INTO messages(state) VALUES ('acked');")
            .unwrap();
        drop(database);

        assert!(matches!(
            snapshot_directory(&source, &destination, false),
            Err(SnapshotError::NodeNotStopped)
        ));
        assert!(!destination.exists());

        let report = snapshot_directory(&source, &destination, true).unwrap();
        assert!(report.completed);
        assert!(report.verified_inventory);
        assert_eq!(report.copied_files, 3);
        assert!(report
            .audit
            .databases
            .iter()
            .any(|db| db.name == "messages.sqlite" && db.integrity_ok == Some(true)));
        assert!(report.audit.databases.iter().any(|db| {
            db.name == "slang-pilot.sqlite" && !db.present && db.integrity_ok.is_none()
        }));
        assert_eq!(
            fs::read(source.join("sessions/session.json")).unwrap(),
            fs::read(destination.join("sessions/session.json")).unwrap()
        );
        assert!(matches!(
            snapshot_directory(&source, &destination, true),
            Err(SnapshotError::UnsafeDestination)
        ));
    }
}
