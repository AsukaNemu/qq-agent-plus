//! Guarded Node-to-RInBot first-import orchestration.
//! Source data is copied unchanged into `legacy/`; normalized chat state is imported
//! into a separate `rinbot.sqlite`, never into or alongside the Node database writer.

use std::{
    fs,
    io::{Read, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    adapters::sqlite::{
        LegacyDomainImportSummary, LegacyImportSummary, LegacySessionImportSummary, SqliteStore,
    },
    application::relationship_pilot::RelationshipPilotSettings,
    application::{
        asr::AsrSettings,
        handoff::ConversationHandoff,
        memory_consolidation::MemoryConsolidationStore,
        model_settings::{FallbackModelSettings, VisionModelSettings},
        proactive::{NodeProactiveRuntimeSnapshot, ProactiveStore},
    },
    daily_moments::{DailyMomentsImportSummary, DailyMomentsStore},
    identity::IdentityStore,
    incident::{IncidentSettings, IncidentStore},
    migration_audit::{
        audit_directory, effective_node_enabled_features, snapshot_directory, MigrationAudit,
        SnapshotError, SnapshotReport,
    },
    migration_json::normalize_node_json,
    qzone_interactions::{QzoneInteractionsImportSummary, QzoneInteractionsStore},
    relationship::{RelationshipImportSummary, RelationshipStore},
    runtime::config::{ImportedNodeRuntimeConfig, IMPORTED_CONFIG_FILE},
    stickers::{StickerImportSummary, StickerStore},
};

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("import requires explicit confirmation that the Node writer is stopped")]
    NodeNotStopped,
    #[error("source directory is missing, unreadable, or contains audit failures")]
    InvalidSource,
    #[error("destination must be a new directory outside the source tree")]
    UnsafeDestination,
    #[error("source snapshot could not be verified")]
    Snapshot(#[from] SnapshotError),
    #[error("normalized import failed; the source was not modified")]
    ImportFailed,
    #[error("normalized import failed during {stage}; the source was not modified")]
    ImportFailedAt { stage: &'static str },
}

fn fingerprint_data_tree(
    root: &Path,
    domain: &[u8],
    exclude_migration_artifacts: bool,
) -> Result<String, ImportError> {
    fn visit(
        root: &Path,
        directory: &Path,
        hasher: &mut Sha256,
        exclude_migration_artifacts: bool,
    ) -> Result<(), ImportError> {
        let mut entries = fs::read_dir(directory)
            .map_err(|_| ImportError::ImportFailed)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ImportError::ImportFailed)?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|_| ImportError::ImportFailed)?;
            if exclude_migration_artifacts
                && (relative == Path::new("legacy")
                    || relative == Path::new("migration-report.json"))
            {
                continue;
            }
            let kind = fs::symlink_metadata(&path)
                .map_err(|_| ImportError::ImportFailed)?
                .file_type();
            if kind.is_symlink() {
                return Err(ImportError::ImportFailed);
            }
            let name = relative.to_str().ok_or(ImportError::ImportFailed)?;
            if kind.is_dir() {
                hasher.update([b'D']);
                hasher.update((name.len() as u64).to_be_bytes());
                hasher.update(name.as_bytes());
                visit(root, &path, hasher, exclude_migration_artifacts)?;
                continue;
            }
            if !kind.is_file() {
                return Err(ImportError::ImportFailed);
            }
            let mut options = fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW);
            }
            let mut file = options.open(&path).map_err(|_| ImportError::ImportFailed)?;
            let metadata = file.metadata().map_err(|_| ImportError::ImportFailed)?;
            if !metadata.is_file() {
                return Err(ImportError::ImportFailed);
            }
            hasher.update([b'F']);
            hasher.update((name.len() as u64).to_be_bytes());
            hasher.update(name.as_bytes());
            hasher.update(metadata.len().to_be_bytes());
            let mut buffer = [0u8; 64 * 1024];
            loop {
                let count = file
                    .read(&mut buffer)
                    .map_err(|_| ImportError::ImportFailed)?;
                if count == 0 {
                    break;
                }
                hasher.update(&buffer[..count]);
            }
        }
        Ok(())
    }

    let metadata = fs::symlink_metadata(root).map_err(|_| ImportError::ImportFailed)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ImportError::ImportFailed);
    }
    let mut hasher = Sha256::new();
    hasher.update(domain);
    visit(root, root, &mut hasher, exclude_migration_artifacts)?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Fingerprint the normalized runtime data without hashing the separately
/// retained legacy archive or this self-describing report.
pub fn fingerprint_normalized_data_tree(root: &Path) -> Result<String, ImportError> {
    fingerprint_data_tree(root, b"RInBot normalized runtime data tree v1\0", true)
}

/// Fingerprint every file in the retained Node archive. Unlike the normalized
/// runtime digest, this deliberately does not exclude a source-owned `legacy/`
/// directory or any other path from the copied tree.
pub fn fingerprint_legacy_archive(root: &Path) -> Result<String, ImportError> {
    fingerprint_data_tree(root, b"RInBot immutable Node legacy archive v1\0", false)
}

#[derive(Clone, Debug, Serialize)]
pub struct DataImportReport {
    pub completed: bool,
    pub legacy_archive: String,
    pub legacy_archive_sha256: String,
    pub runtime_database: String,
    pub copied_files: u64,
    pub copied_bytes: u64,
    pub source_audit: MigrationAudit,
    pub normalized: LegacyImportSummary,
    pub sessions: LegacySessionImportSummary,
    pub memory_handoffs: MemoryHandoffImportSummary,
    pub global_person_memory: GlobalPersonMemoryImportSummary,
    pub memory_consolidation_backups: MemoryConsolidationBackupArchiveSummary,
    pub memory_consolidation_cooldowns: MemoryConsolidationCooldownImportSummary,
    pub proactive_state: ProactiveStateImportSummary,
    pub daily_moments: DailyMomentsImportSummary,
    pub daily_moments_archive_present: bool,
    pub enabled_daily_moments_data_missing: bool,
    pub qzone_interactions: QzoneInteractionsImportSummary,
    pub qzone_interactions_archive_present: bool,
    pub enabled_qzone_interactions_data_missing: bool,
    pub stickers: StickerImportSummary,
    pub sticker_archive_present: bool,
    pub enabled_sticker_data_missing: bool,
    pub sticker_assets_copied: u64,
    pub pilot_databases: Vec<LegacyDomainImportSummary>,
    pub target_integrity_ok: bool,
    pub target_data_sha256: String,
    pub verified_message_samples: u64,
    pub verified_outbox_samples: u64,
    pub incident_records_imported: u64,
    pub incident_chat_controls_imported: u64,
    pub relationship_records_imported: RelationshipImportSummary,
    /// The first-pass core mapping is distinct from advanced feature parity.
    pub configuration: ConfigurationImportReview,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct GlobalPersonMemoryImportSummary {
    pub source_files: u64,
    pub source_impressions: u64,
    pub resolved_name_only_files: u64,
    pub archived_only_files: u64,
    pub imported_chat_scoped_rows: u64,
    pub target_unique_rows: u64,
    pub verified_rows: u64,
    pub verified_samples: u64,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct MemoryHandoffImportSummary {
    pub source_json_files: u64,
    pub imported: u64,
    pub expired: u64,
    pub verified_rows: u64,
    pub verified_samples: u64,
    pub legacy_state_files: u64,
    pub converted_legacy_states: u64,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct MemoryConsolidationCooldownImportSummary {
    pub source_chat_metadata_files: u64,
    pub source_person_files: u64,
    pub imported_chat_cooldowns: u64,
    pub imported_person_cooldowns: u64,
    pub imported_person_updated_rows: u64,
    pub verified_chat_rows: u64,
    pub verified_person_cooldown_rows: u64,
    pub verified_person_updated_rows: u64,
    pub verified_samples: u64,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct ProactiveStateImportSummary {
    pub source_present: bool,
    pub imported: bool,
    pub verified: bool,
    pub runtime_snapshot_present: bool,
    pub source_scheduled_wakes: u64,
    pub imported_scheduled_wakes: u64,
    pub verified_scheduled_wakes: u64,
    pub source_follow_up_cooldowns: u64,
    pub imported_follow_up_cooldowns: u64,
    pub verified_follow_up_cooldowns: u64,
}

/// Node's immutable pre-write snapshots are retained byte-for-byte under
/// `legacy/`. Exact QQ identities are imported directly; name-only snapshots
/// are indexed only when their name/alias uniquely resolves in their source
/// chat, otherwise the report keeps them archived-only and blocks the feature.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct MemoryConsolidationBackupArchiveSummary {
    pub archive_present: bool,
    pub source_files: u64,
    pub verified_files: u64,
    pub person_snapshots: u64,
    pub impressions: u64,
    pub restorable_files: u64,
    pub resolved_name_only_files: u64,
    pub imported_restorable_files: u64,
    pub archived_only_files: u64,
    pub preserved_in_legacy_archive: bool,
    pub available_to_rust_restore_ui: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ConfigurationImportReview {
    pub legacy_config_present: bool,
    pub legacy_config_valid_json: bool,
    pub secrets_exposed_in_report: bool,
    pub core_runtime_configuration_mapped: bool,
    /// Complete effective Node feature inventory from the source configuration.
    pub enabled_features: Vec<String>,
    /// Enabled features whose configured behavior is not covered by this Rust build.
    pub unmapped_enabled_features: Vec<String>,
    pub enabled_pilot_data_missing: Vec<String>,
    /// Enabled modules whose persistent source archive/database was absent.
    /// This is copied to the private runtime manifest and blocks active mode.
    pub enabled_feature_data_missing: Vec<String>,
}

/// Create a complete new RInBot data root from a stopped Node data root.
/// Existing destinations are never overwritten; all work happens in a private
/// sibling staging directory and becomes visible through one final rename.
pub fn import_stopped_node_directory(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
    node_stopped: bool,
) -> Result<DataImportReport, ImportError> {
    if !node_stopped {
        return Err(ImportError::NodeNotStopped);
    }
    let source = source
        .as_ref()
        .canonicalize()
        .map_err(|_| ImportError::InvalidSource)?;
    if !source.is_dir() {
        return Err(ImportError::InvalidSource);
    }
    let audit = audit_directory(&source);
    if !audit.failures.is_empty() {
        return Err(ImportError::InvalidSource);
    }
    let destination_arg = destination.as_ref();
    if destination_arg.exists() {
        return Err(ImportError::UnsafeDestination);
    }
    let name = destination_arg
        .file_name()
        .ok_or(ImportError::UnsafeDestination)?;
    let parent = destination_arg
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()
        .map_err(|_| ImportError::UnsafeDestination)?;
    let destination = parent.join(name);
    if destination == source || destination.starts_with(&source) {
        return Err(ImportError::UnsafeDestination);
    }
    let staging = parent.join(format!(".rinbot-import-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&staging).map_err(|_| ImportError::ImportFailed)?;
    set_directory_private(&staging).map_err(|_| {
        let _ = fs::remove_dir_all(&staging);
        ImportError::ImportFailed
    })?;

    let mut stage = "legacy snapshot";
    let result = build_import(&source, &staging, &audit, &mut stage);
    match result {
        Ok(mut report) => {
            report.completed = true;
            if restrict_tree_permissions(&staging).is_err() {
                let _ = fs::remove_dir_all(&staging);
                return Err(ImportError::ImportFailed);
            }
            if destination.exists() {
                let _ = fs::remove_dir_all(&staging);
                return Err(ImportError::UnsafeDestination);
            }
            fs::rename(&staging, &destination).map_err(|_| ImportError::ImportFailed)?;
            Ok(report)
        }
        Err(ImportError::ImportFailed) => {
            let _ = fs::remove_dir_all(&staging);
            Err(ImportError::ImportFailedAt { stage })
        }
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            Err(error)
        }
    }
}

fn build_import(
    source: &Path,
    staging: &Path,
    audit: &MigrationAudit,
    stage: &mut &'static str,
) -> Result<DataImportReport, ImportError> {
    let legacy_destination = staging.join("legacy");
    let snapshot: SnapshotReport = snapshot_directory(source, &legacy_destination, true)?;
    if snapshot.audit != *audit || !snapshot.verified_inventory {
        return Err(ImportError::ImportFailed);
    }
    let legacy_archive_sha256 = fingerprint_legacy_archive(&legacy_destination)?;
    let mut configuration = import_core_runtime_config(
        &legacy_destination.join("config.json"),
        &staging.join(IMPORTED_CONFIG_FILE),
    );
    let runtime_db = staging.join("rinbot.sqlite");
    *stage = "normalized core database";
    let store = SqliteStore::open(&runtime_db).map_err(|_| ImportError::ImportFailed)?;
    let source_inventory = serde_json::to_string(audit).map_err(|_| ImportError::ImportFailed)?;
    *stage = "message and outbox archive";
    let normalized = store
        .import_node_messages(
            legacy_destination.join("messages.sqlite"),
            &source_inventory,
        )
        .map_err(|_| ImportError::ImportFailed)?;
    *stage = "session summaries";
    let sessions = store
        .import_legacy_session_summaries(legacy_destination.join("sessions"))
        .map_err(|_| ImportError::ImportFailed)?;
    *stage = "memory handoffs";
    let memory_handoffs = import_memory_handoffs(&legacy_destination.join("memory"), &store)?;
    *stage = "person memories";
    let global_person_memory = import_global_person_memories(
        &legacy_destination.join("memory/people"),
        &staging.join("identity.sqlite"),
        &legacy_destination.join("identity-pilot.sqlite"),
        current_time_ms(),
    )?;
    *stage = "memory consolidation backups";
    let memory_consolidation_backups = import_memory_consolidation_backup_archive(
        &legacy_destination.join("memory/backups/consolidation"),
        &staging.join("identity.sqlite"),
        &legacy_destination.join("identity-pilot.sqlite"),
        current_time_ms(),
    )?;
    *stage = "memory consolidation cooldowns";
    let memory_consolidation_cooldowns =
        ImportedNodeRuntimeConfig::read(&staging.join(IMPORTED_CONFIG_FILE))
            .ok()
            .filter(|config| config.memory_consolidate_enabled.unwrap_or(false))
            .map(|_| {
                import_memory_consolidation_cooldowns(
                    &legacy_destination.join("memory"),
                    &legacy_destination.join("identity-pilot.sqlite"),
                    &staging.join("memory-consolidation.sqlite"),
                    true,
                )
            })
            .transpose()?
            .unwrap_or_default();
    *stage = "proactive state";
    let proactive_enabled = configuration
        .enabled_features
        .iter()
        .any(|feature| matches!(feature.as_str(), "proactive" | "followUp" | "selfWake"));
    let proactive_state = import_proactive_state(
        &legacy_destination.join("proactive-state.json"),
        &staging.join("proactive.sqlite"),
        proactive_enabled,
    )?;
    let enabled_proactive_runtime_snapshot_missing =
        proactive_enabled && !proactive_state.runtime_snapshot_present;
    *stage = "daily moments archive";
    let daily_moments_archive = legacy_destination.join("daily-moments.json");
    let daily_moments_archive_present = daily_moments_archive.is_file();
    let daily_moments = if daily_moments_archive_present {
        let mut moments = DailyMomentsStore::open(staging.join("daily-moments.sqlite"))
            .map_err(|_| ImportError::ImportFailed)?;
        let imported = moments
            .import_node_archive(&daily_moments_archive)
            .map_err(|_| ImportError::ImportFailed)?;
        let counts = moments.counts().map_err(|_| ImportError::ImportFailed)?;
        if !moments
            .integrity_ok()
            .map_err(|_| ImportError::ImportFailed)?
            || counts != (imported.imported_records, imported.imported_schedule_slots)
            || imported.source_records != imported.imported_records
            || imported.source_schedule_slots != imported.imported_schedule_slots
            || imported.verified_record_rows != imported.imported_records
            || imported.verified_schedule_rows != imported.imported_schedule_slots
            || imported.verified_record_samples != imported.imported_records.min(3)
            || imported.verified_schedule_samples != imported.imported_schedule_slots.min(3)
        {
            return Err(ImportError::ImportFailed);
        }
        drop(moments);
        restrict_file_permissions(&staging.join("daily-moments.sqlite"))
            .map_err(|_| ImportError::ImportFailed)?;
        imported
    } else {
        DailyMomentsImportSummary::default()
    };
    let enabled_daily_moments_data_missing = !daily_moments_archive_present
        && configuration
            .enabled_features
            .iter()
            .any(|feature| feature == "dailyMoments");
    *stage = "QZone interaction archive";
    let qzone_interactions_archive = legacy_destination.join("qzone-interactions.json");
    let qzone_interactions_archive_present = qzone_interactions_archive.is_file();
    let qzone_interactions = if qzone_interactions_archive_present {
        let mut interactions =
            QzoneInteractionsStore::open(staging.join("qzone-interactions.sqlite"))
                .map_err(|_| ImportError::ImportFailed)?;
        let imported = interactions
            .import_node_archive(&qzone_interactions_archive)
            .map_err(|_| ImportError::ImportFailed)?;
        if interactions
            .counts()
            .map_err(|_| ImportError::ImportFailed)?
            != imported.imported_items
            || !interactions
                .integrity_ok()
                .map_err(|_| ImportError::ImportFailed)?
            || imported.source_items != imported.imported_items
            || imported.verified_item_rows != imported.imported_items
            || imported.source_metadata_entries != imported.imported_metadata_entries
            || imported.imported_metadata_entries != imported.verified_metadata_rows
            || imported.verified_samples != imported.imported_items.min(3)
        {
            return Err(ImportError::ImportFailed);
        }
        drop(interactions);
        restrict_file_permissions(&staging.join("qzone-interactions.sqlite"))
            .map_err(|_| ImportError::ImportFailed)?;
        imported
    } else {
        QzoneInteractionsImportSummary::default()
    };
    let enabled_qzone_interactions_data_missing = !qzone_interactions_archive_present
        && configuration
            .enabled_features
            .iter()
            .any(|feature| feature == "qzoneInteractions");
    *stage = "sticker archive and media";
    let sticker_archive = legacy_destination.join("stickers.json");
    let sticker_archive_present = sticker_archive.is_file();
    let mut sticker_assets_copied = 0;
    let stickers = if sticker_archive_present {
        let mut store = StickerStore::open(staging.join("stickers.sqlite"), true)
            .map_err(|_| ImportError::ImportFailed)?
            .ok_or(ImportError::ImportFailed)?;
        let imported = store
            .import_node_archive(&sticker_archive)
            .map_err(|_| ImportError::ImportFailed)?;
        if store.count().map_err(|_| ImportError::ImportFailed)? != imported.imported_entries
            || imported.source_entries != imported.imported_entries
            || imported.verified_rows != imported.source_entries
            || imported.verified_samples != imported.imported_entries.min(3)
            || !store
                .integrity_ok()
                .map_err(|_| ImportError::ImportFailed)?
        {
            return Err(ImportError::ImportFailed);
        }
        sticker_assets_copied = copy_sticker_assets(&mut store, &legacy_destination, staging)?;
        drop(store);
        restrict_file_permissions(&staging.join("stickers.sqlite"))
            .map_err(|_| ImportError::ImportFailed)?;
        imported
    } else {
        StickerImportSummary::default()
    };
    let enabled_sticker_data_missing = !sticker_archive_present
        && configuration
            .enabled_features
            .iter()
            .any(|feature| feature == "sticker" || feature == "stickerCollect");
    *stage = "pilot database archives";
    let mut pilot_databases = Vec::new();
    for (feature_name, source_name, domain, required_tables) in [
        (
            "identityPilot",
            "identity-pilot.sqlite",
            "identity",
            &[
                "people",
                "identity_sources",
                "identity_aliases",
                "incoming_friend_requests",
            ][..],
        ),
        (
            "incidentPilot",
            "incident-pilot.sqlite",
            "incident",
            &["incidents"][..],
        ),
        (
            "relationshipPilot",
            "relationship-pilot.sqlite",
            "relationship",
            &[
                "relationship_states",
                "relationship_events",
                "relationship_flags",
                "relationship_cursors",
                "relationship_evaluations",
            ][..],
        ),
    ] {
        let source_path = legacy_destination.join(source_name);
        if source_path.is_file() {
            pilot_databases.push(
                store
                    .import_legacy_domain_database(&source_path, domain, required_tables)
                    .map_err(|_| ImportError::ImportFailed)?,
            );
        } else if configuration
            .enabled_features
            .iter()
            .any(|feature| feature == feature_name)
        {
            configuration
                .enabled_pilot_data_missing
                .push(feature_name.to_owned());
        }
    }
    *stage = "incident records and controls";
    let incident_archive = legacy_destination.join("incident-pilot.sqlite");
    let (incident_records_imported, incident_chat_controls_imported) = if incident_archive.is_file()
    {
        let manifest = ImportedNodeRuntimeConfig::read(&staging.join(IMPORTED_CONFIG_FILE))
            .map_err(|_| ImportError::ImportFailed)?;
        // Existing history remains inspectable when the imported runtime switch
        // is off. This one-time import only creates a feature DB because the
        // source DB exists; normal disabled startup mounts it read-only.
        let incidents = IncidentStore::open(
            staging.join("incident-pilot.sqlite"),
            IncidentSettings {
                enabled: true,
                graduated: manifest.incident_pilot_graduated.unwrap_or(false),
                notify_warnings: manifest.incident_notify_warnings.unwrap_or(true),
                duplicate_window_minutes: manifest.incident_duplicate_window_minutes.unwrap_or(10),
                retention_days: manifest.incident_retention_days.unwrap_or(90),
                unknown_writes_block_chat: manifest
                    .incident_unknown_writes_block_chat
                    .unwrap_or(false),
            },
        )
        .map_err(|_| ImportError::ImportFailed)?;
        let incidents_imported = incidents
            .import_legacy_database(&incident_archive)
            .map_err(|_| ImportError::ImportFailed)?;
        let chat_controls_imported = incidents
            .import_legacy_chat_controls(&incident_archive)
            .map_err(|_| ImportError::ImportFailed)?;
        (incidents_imported, chat_controls_imported)
    } else {
        (0, 0)
    };
    *stage = "relationship records and cursors";
    let relationship_archive = legacy_destination.join("relationship-pilot.sqlite");
    let relationship_records_imported = if relationship_archive.is_file() {
        let mut relationships = RelationshipStore::open(staging.join("relationship-pilot.sqlite"))
            .map_err(|_| ImportError::ImportFailed)?;
        let states = read_all_legacy_records(&store, "relationship", "relationship_states")?;
        let events = read_all_legacy_records(&store, "relationship", "relationship_events")?;
        let flags = read_all_legacy_records(&store, "relationship", "relationship_flags")?;
        let mut cursors = read_all_legacy_records(&store, "relationship", "relationship_cursors")?;
        // Node cursors use a message id local to each chat; the Rust inbox
        // assigns one global id. Translate exact archived evidence rows before
        // installing cursors so first Rust evaluation starts strictly after
        // already-consumed messages. If a cursor row is absent, reset to zero:
        // bounded re-reading is safer than silently skipping new evidence.
        for cursor in &mut cursors {
            let Some(object) = cursor.as_object_mut() else {
                return Err(ImportError::ImportFailed);
            };
            let uin = object
                .get("uin")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let chat_key = object
                .get("chat_key")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let source_id = object
                .get("last_message_id")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            let target_id = if source_id > 0 {
                store
                    .legacy_message_row_id(chat_key, uin, source_id)
                    .map_err(|_| ImportError::ImportFailed)?
                    .unwrap_or_default()
            } else {
                0
            };
            object.insert("last_message_id".into(), Value::from(target_id));
        }
        let evaluations =
            read_all_legacy_records(&store, "relationship", "relationship_evaluations")?;
        let imported = relationships
            .import_legacy_records(
                "node-relationship-pilot-v1",
                &states,
                &events,
                &flags,
                &cursors,
                &evaluations,
                current_time_ms(),
            )
            .map_err(|_| ImportError::ImportFailed)?;
        if !relationships
            .integrity_ok()
            .map_err(|_| ImportError::ImportFailed)?
        {
            return Err(ImportError::ImportFailed);
        }
        let target_counts = relationships
            .counts()
            .map_err(|_| ImportError::ImportFailed)?;
        if target_counts.states != states.len() as u64
            || target_counts.events != events.len() as u64
            || target_counts.flags != flags.len() as u64
            || target_counts.cursors != cursors.len() as u64
            || target_counts.evaluations != evaluations.len() as u64
        {
            return Err(ImportError::ImportFailed);
        }
        imported
    } else {
        RelationshipImportSummary::default()
    };
    *stage = "feature data gates and imported manifest";
    let mut enabled_feature_data_missing = configuration.enabled_pilot_data_missing.clone();
    if configuration
        .enabled_features
        .iter()
        .any(|feature| feature == "memoryConsolidate")
        && (!memory_consolidation_backups.available_to_rust_restore_ui
            || global_person_memory.archived_only_files > 0)
    {
        enabled_feature_data_missing.push("memoryConsolidate".into());
    }
    if enabled_feature_data_missing
        .iter()
        .any(|feature| feature == "identityPilot")
        && configuration
            .enabled_features
            .iter()
            .any(|feature| feature == "incomingFriendRequest")
    {
        enabled_feature_data_missing.push("incomingFriendRequest".into());
    }
    if enabled_daily_moments_data_missing {
        enabled_feature_data_missing.push("dailyMoments".into());
    }
    if enabled_qzone_interactions_data_missing {
        enabled_feature_data_missing.push("qzoneInteractions".into());
    }
    if enabled_sticker_data_missing {
        enabled_feature_data_missing.extend(
            configuration
                .enabled_features
                .iter()
                .filter(|feature| {
                    feature.as_str() == "sticker" || feature.as_str() == "stickerCollect"
                })
                .cloned(),
        );
    }
    if enabled_proactive_runtime_snapshot_missing {
        enabled_feature_data_missing.extend(
            configuration
                .enabled_features
                .iter()
                .filter(|feature| matches!(feature.as_str(), "proactive" | "followUp" | "selfWake"))
                .cloned(),
        );
    }
    enabled_feature_data_missing.sort();
    enabled_feature_data_missing.dedup();
    configuration.enabled_feature_data_missing = enabled_feature_data_missing.clone();
    let imported_config_path = staging.join(IMPORTED_CONFIG_FILE);
    let mut imported_config = ImportedNodeRuntimeConfig::read(&imported_config_path)
        .map_err(|_| ImportError::ImportFailed)?;
    imported_config.enabled_feature_data_missing = Some(enabled_feature_data_missing);
    fs::write(
        &imported_config_path,
        serde_json::to_vec_pretty(&imported_config).map_err(|_| ImportError::ImportFailed)?,
    )
    .map_err(|_| ImportError::ImportFailed)?;
    *stage = "target count and outbox verification";
    let database_counts = store.counts().map_err(|_| ImportError::ImportFailed)?;
    if database_counts.messages != normalized.messages as i64 {
        return Err(ImportError::ImportFailed);
    }
    let expected_unknown = normalized
        .outbox_states
        .get("unknown")
        .copied()
        .unwrap_or_default()
        + normalized
            .outbox_states
            .get("sending")
            .copied()
            .unwrap_or_default();
    if database_counts.unknown_outbox != expected_unknown as i64 {
        return Err(ImportError::ImportFailed);
    }
    drop(store);
    restrict_file_permissions(&runtime_db).map_err(|_| ImportError::ImportFailed)?;
    *stage = "final integrity and sample verification";
    let (target_integrity_ok, verified_message_samples, verified_outbox_samples) =
        verify_target(&runtime_db, &normalized, &sessions, &pilot_databases)
            .map_err(|_| ImportError::ImportFailed)?;
    let expected_session_samples = sessions.imported.min(3);
    if !target_integrity_ok
        || verified_message_samples != normalized.verified_message_samples
        || verified_outbox_samples != normalized.verified_outbox_samples
        || sessions.imported != sessions.source_json_files
        || sessions.verified_rows != sessions.imported
        || sessions.verified_samples != expected_session_samples
        || memory_handoffs.imported + memory_handoffs.expired != memory_handoffs.source_json_files
        || memory_handoffs.verified_rows
            != memory_handoffs.imported + memory_handoffs.converted_legacy_states
        || memory_handoffs.verified_samples
            != memory_handoffs
                .imported
                .saturating_add(memory_handoffs.converted_legacy_states)
                .min(3)
        || global_person_memory.verified_rows != global_person_memory.target_unique_rows
        || memory_consolidation_cooldowns.verified_chat_rows
            != memory_consolidation_cooldowns.imported_chat_cooldowns
        || memory_consolidation_cooldowns.verified_person_cooldown_rows
            != memory_consolidation_cooldowns.imported_person_cooldowns
        || memory_consolidation_cooldowns.verified_person_updated_rows
            != memory_consolidation_cooldowns.imported_person_updated_rows
        || memory_consolidation_backups.verified_files != memory_consolidation_backups.source_files
        || memory_consolidation_backups.imported_restorable_files
            != memory_consolidation_backups.restorable_files
        || (verified_message_samples == 0 && normalized.messages > 0)
        || (verified_outbox_samples == 0 && normalized.outbox > 0)
    {
        return Err(ImportError::ImportFailed);
    }

    let target_data_sha256 = fingerprint_normalized_data_tree(staging)?;

    let report = DataImportReport {
        completed: true,
        legacy_archive: "legacy".into(),
        legacy_archive_sha256,
        runtime_database: "rinbot.sqlite".into(),
        copied_files: snapshot.copied_files,
        copied_bytes: snapshot.copied_bytes,
        source_audit: audit.clone(),
        normalized,
        sessions,
        memory_handoffs,
        global_person_memory,
        memory_consolidation_backups,
        memory_consolidation_cooldowns,
        proactive_state,
        daily_moments,
        daily_moments_archive_present,
        enabled_daily_moments_data_missing,
        qzone_interactions,
        qzone_interactions_archive_present,
        enabled_qzone_interactions_data_missing,
        stickers,
        sticker_archive_present,
        enabled_sticker_data_missing,
        sticker_assets_copied,
        pilot_databases,
        target_integrity_ok,
        target_data_sha256,
        verified_message_samples,
        verified_outbox_samples,
        incident_records_imported,
        incident_chat_controls_imported,
        relationship_records_imported,
        configuration,
    };
    let report_path = staging.join("migration-report.json");
    let serialized = serde_json::to_vec_pretty(&report).map_err(|_| ImportError::ImportFailed)?;
    let mut output = private_new_file(&report_path).map_err(|_| ImportError::ImportFailed)?;
    output
        .write_all(&serialized)
        .map_err(|_| ImportError::ImportFailed)?;
    output.flush().map_err(|_| ImportError::ImportFailed)?;
    Ok(report)
}

fn import_proactive_state(
    source: &Path,
    destination: &Path,
    enabled: bool,
) -> Result<ProactiveStateImportSummary, ImportError> {
    if !source.is_file() {
        return Ok(ProactiveStateImportSummary::default());
    }
    const MAX_PROACTIVE_STATE_BYTES: u64 = 8 * 1024 * 1024;
    if fs::metadata(source)
        .map_err(|_| ImportError::ImportFailed)?
        .len()
        > MAX_PROACTIVE_STATE_BYTES
    {
        return Err(ImportError::ImportFailed);
    }
    let bytes = fs::read(source).map_err(|_| ImportError::ImportFailed)?;
    let normalized_bytes = normalize_node_json(&bytes);
    let value: Value =
        serde_json::from_slice(&normalized_bytes).map_err(|_| ImportError::ImportFailed)?;
    let last_attempt_at = value
        .get("lastAttemptAt")
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|text| text.parse::<i64>().ok()))
        })
        .unwrap_or_default();
    let runtime_snapshot = match value.get("runtimeSnapshot") {
        Some(snapshot) => Some(
            serde_json::from_value::<NodeProactiveRuntimeSnapshot>(snapshot.clone())
                .map_err(|_| ImportError::ImportFailed)?,
        ),
        None => None,
    };
    if !enabled {
        return Ok(ProactiveStateImportSummary {
            source_present: true,
            imported: false,
            verified: false,
            runtime_snapshot_present: runtime_snapshot.is_some(),
            ..ProactiveStateImportSummary::default()
        });
    }
    let mut store = ProactiveStore::open(destination, true)
        .map_err(|_| ImportError::ImportFailed)?
        .ok_or(ImportError::ImportFailed)?;
    store
        .record_proactive_attempt(last_attempt_at)
        .map_err(|_| ImportError::ImportFailed)?;
    let verified = store
        .last_proactive_attempt_at()
        .map(|value| value == last_attempt_at)
        .unwrap_or(false);
    if !verified {
        return Err(ImportError::ImportFailed);
    }
    let (source_scheduled_wakes, imported_scheduled_wakes, verified_scheduled_wakes) =
        if let Some(snapshot) = runtime_snapshot.as_ref() {
            let source_wakes = snapshot.scheduled_wakes.len() as u64;
            let source_cooldowns = snapshot.follow_up_at.len() as u64;
            let (imported_wakes, imported_cooldowns) = store
                .import_node_runtime_snapshot(snapshot)
                .map_err(|_| ImportError::ImportFailed)?;
            if source_wakes != imported_wakes || source_cooldowns != imported_cooldowns {
                return Err(ImportError::ImportFailed);
            }
            (source_wakes, imported_wakes, imported_wakes)
        } else {
            (0, 0, 0)
        };
    let (source_follow_up_cooldowns, imported_follow_up_cooldowns, verified_follow_up_cooldowns) =
        if let Some(snapshot) = runtime_snapshot.as_ref() {
            let count = snapshot.follow_up_at.len() as u64;
            (count, count, count)
        } else {
            (0, 0, 0)
        };
    Ok(ProactiveStateImportSummary {
        source_present: true,
        imported: true,
        verified,
        runtime_snapshot_present: runtime_snapshot.is_some(),
        source_scheduled_wakes,
        imported_scheduled_wakes,
        verified_scheduled_wakes,
        source_follow_up_cooldowns,
        imported_follow_up_cooldowns,
        verified_follow_up_cooldowns,
    })
}

fn copy_sticker_assets(
    store: &StickerStore,
    legacy_root: &Path,
    staging_root: &Path,
) -> Result<u64, ImportError> {
    let source_assets = legacy_root.join("sticker-assets");
    let total = store.count().map_err(|_| ImportError::ImportFailed)? as usize;
    let mut offset = 0;
    let mut copied = std::collections::BTreeSet::new();
    while offset < total {
        let page = store
            .list("", 500, offset)
            .map_err(|_| ImportError::ImportFailed)?;
        if page.is_empty() {
            break;
        }
        offset += page.len();
        for entry in page {
            let Some(file_name) = entry.local_file.strip_prefix("sticker-assets/") else {
                continue;
            };
            if !copied.insert(file_name.to_owned()) {
                continue;
            }
            let directory_metadata =
                fs::symlink_metadata(&source_assets).map_err(|_| ImportError::ImportFailed)?;
            if !directory_metadata.file_type().is_dir() {
                return Err(ImportError::ImportFailed);
            }
            let source = source_assets.join(file_name);
            let source_metadata =
                fs::symlink_metadata(&source).map_err(|_| ImportError::ImportFailed)?;
            if !source_metadata.file_type().is_file() || source_metadata.len() > 8 * 1024 * 1024 {
                return Err(ImportError::ImportFailed);
            }
            let target_directory = staging_root.join("sticker-assets");
            if !target_directory.exists() {
                fs::create_dir(&target_directory).map_err(|_| ImportError::ImportFailed)?;
                set_directory_private(&target_directory).map_err(|_| ImportError::ImportFailed)?;
            }
            let bytes = fs::read(source).map_err(|_| ImportError::ImportFailed)?;
            let target = target_directory.join(file_name);
            let mut output = private_new_file(&target).map_err(|_| ImportError::ImportFailed)?;
            output
                .write_all(&bytes)
                .map_err(|_| ImportError::ImportFailed)?;
            output.flush().map_err(|_| ImportError::ImportFailed)?;
            restrict_file_permissions(&target).map_err(|_| ImportError::ImportFailed)?;
        }
    }
    Ok(copied.len() as u64)
}

fn import_memory_handoffs(
    memory_dir: &Path,
    store: &SqliteStore,
) -> Result<MemoryHandoffImportSummary, ImportError> {
    let mut summary = MemoryHandoffImportSummary::default();
    if !memory_dir.exists() {
        return Ok(summary);
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ImportError::ImportFailed)?
        .as_millis() as i64;
    let mut pending = vec![memory_dir.to_path_buf()];
    let mut verified = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).map_err(|_| ImportError::ImportFailed)? {
            let entry = entry.map_err(|_| ImportError::ImportFailed)?;
            let kind = entry.file_type().map_err(|_| ImportError::ImportFailed)?;
            if kind.is_symlink() {
                return Err(ImportError::ImportFailed);
            }
            if kind.is_dir() {
                pending.push(entry.path());
                continue;
            }
            if !kind.is_file() || entry.file_name() != "_handoff.json" {
                continue;
            }
            summary.source_json_files += 1;
            let chat_key = entry
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
                    .then(|| format!("{kind}:{id}"))
                })
                .ok_or(ImportError::ImportFailed)?;
            let raw = fs::read_to_string(entry.path()).map_err(|_| ImportError::ImportFailed)?;
            let handoff = ConversationHandoff::from_legacy_json(&raw)
                .map_err(|_| ImportError::ImportFailed)?;
            if handoff.is_expired(now) {
                summary.expired += 1;
                continue;
            }
            store
                .save_conversation_handoff(&chat_key, &handoff)
                .map_err(|_| ImportError::ImportFailed)?;
            summary.imported += 1;
            verified.push((chat_key, handoff));
        }
    }
    // Node lazily converts legacy root-level group_<id>.json/private_<id>.json
    // state into a 24-hour handoff when MemoryStore first scans source chats.
    // Reproduce that conversion in the isolated import tree without starting
    // Node or changing the source snapshot. An existing per-chat handoff wins,
    // exactly as migrateLegacyHandoff does.
    let mut legacy_files = fs::read_dir(memory_dir)
        .map_err(|_| ImportError::ImportFailed)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ImportError::ImportFailed)?;
    legacy_files.sort_by_key(|entry| entry.file_name());
    for entry in legacy_files {
        let file_type = entry.file_type().map_err(|_| ImportError::ImportFailed)?;
        if file_type.is_symlink() {
            return Err(ImportError::ImportFailed);
        }
        if !file_type.is_file() {
            continue;
        }
        let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(stem) = file_name.strip_suffix(".json") else {
            continue;
        };
        let Some(chat_key) = valid_legacy_chat_directory(stem) else {
            continue;
        };
        summary.legacy_state_files += 1;
        let raw = fs::read_to_string(entry.path()).map_err(|_| ImportError::ImportFailed)?;
        let value: Value = serde_json::from_str(&raw).map_err(|_| ImportError::ImportFailed)?;
        let Some(object) = value.as_object() else {
            continue;
        };
        let chat_directory = memory_dir.join(stem);
        let current_handoff = chat_directory.join("_handoff.json");
        if current_handoff.exists() {
            continue;
        }
        let topic = legacy_state_text(object.get("activeTopic"));
        let pending = legacy_state_text(object.get("pendingThought"));
        if topic.is_empty() && pending.is_empty() {
            continue;
        }
        let mut handoff = ConversationHandoff {
            topic: topic.chars().take(200).collect(),
            summary: if pending.is_empty() {
                topic.chars().take(1_200).collect()
            } else {
                pending.chars().take(1_200).collect()
            },
            next_step: pending.chars().take(400).collect(),
            source_session_id: "legacy-migration".into(),
            updated_at: now,
            expires_at: now.saturating_add(24 * 60 * 60 * 1000),
            ..Default::default()
        };
        handoff.normalize();
        store
            .save_conversation_handoff(&chat_key, &handoff)
            .map_err(|_| ImportError::ImportFailed)?;
        summary.converted_legacy_states += 1;
        verified.push((chat_key, handoff));
    }
    // Every saved handoff, including converted legacy state, must match its
    // persisted runtime projection before the destination can be published.
    for (chat_key, expected) in &verified {
        let actual = store
            .conversation_handoff(chat_key, now)
            .map_err(|_| ImportError::ImportFailed)?;
        if actual.as_ref() != Some(expected) {
            return Err(ImportError::ImportFailed);
        }
        summary.verified_rows += 1;
    }
    summary.verified_samples = summary.verified_rows.min(3);
    Ok(summary)
}

fn legacy_state_text(value: Option<&Value>) -> String {
    fn js_truthy(value: &Value) -> bool {
        match value {
            Value::Null => false,
            Value::Bool(value) => *value,
            Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
            Value::String(value) => !value.is_empty(),
            Value::Array(_) | Value::Object(_) => true,
        }
    }
    fn js_string(value: &Value) -> String {
        match value {
            Value::Null => String::new(),
            Value::String(value) => value.clone(),
            Value::Bool(value) => value.to_string(),
            Value::Number(value) => value.to_string(),
            Value::Array(values) => values.iter().map(js_string).collect::<Vec<_>>().join(","),
            Value::Object(_) => "[object Object]".into(),
        }
    }
    let value = match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .get("content")
                    .filter(|value| js_truthy(value))
                    .or_else(|| value.get("text").filter(|value| js_truthy(value)))
                    .map(js_string)
                    .unwrap_or_else(|| js_string(value))
            })
            .collect::<Vec<_>>()
            .join("；"),
        Some(Value::Object(object)) => ["summary", "content", "text"]
            .iter()
            .filter_map(|key| object.get(*key))
            .find(|value| js_truthy(value))
            .map(js_string)
            .unwrap_or_default(),
        Some(value) => js_string(value),
        None => String::new(),
    };
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(1_000)
        .collect()
}

fn import_global_person_memories(
    people_dir: &Path,
    target_path: &Path,
    identity_source_path: &Path,
    now: i64,
) -> Result<GlobalPersonMemoryImportSummary, ImportError> {
    let mut summary = GlobalPersonMemoryImportSummary::default();
    if !people_dir.exists() {
        return Ok(summary);
    }
    let metadata = fs::symlink_metadata(people_dir).map_err(|_| ImportError::ImportFailed)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ImportError::ImportFailed);
    }
    let mut files = fs::read_dir(people_dir)
        .map_err(|_| ImportError::ImportFailed)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ImportError::ImportFailed)?;
    files.sort_by_key(|entry| entry.file_name());
    let mut rows = Vec::new();
    for entry in files {
        let file_name = entry
            .file_name()
            .to_str()
            .map(str::to_owned)
            .ok_or(ImportError::ImportFailed)?;
        if !file_name.ends_with(".json") {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path()).map_err(|_| ImportError::ImportFailed)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(ImportError::ImportFailed);
        }
        summary.source_files += 1;
        let person = read_import_json(&entry.path())?;
        if !is_importable_person_memory_record(&person) {
            return Err(ImportError::ImportFailed);
        }
        let object = person.as_object().ok_or(ImportError::ImportFailed)?;
        let claimed_user_id = object
            .get("userId")
            .and_then(|value| match value {
                Value::String(value) => Some(value.clone()),
                Value::Number(value) => Some(value.to_string()),
                _ => None,
            })
            .unwrap_or_default();
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let member_sources = object
            .get("sourceChatKeys")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|chat| valid_legacy_chat_key_str(chat).is_some())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let numeric_file_user = file_name
            .strip_suffix(".json")
            .filter(|stem| valid_legacy_user_id(stem))
            .map(str::to_owned);
        let impressions = object
            .get("impressions")
            .and_then(Value::as_array)
            .ok_or(ImportError::ImportFailed)?;
        let mut source_chats = member_sources
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        for impression in impressions {
            if let Some(item) = impression.as_object() {
                source_chats.extend(
                    item.get("sourceChatKeys")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .filter_map(valid_legacy_chat_key_str),
                );
                if let Some(source) = item
                    .get("sourceChatKey")
                    .and_then(Value::as_str)
                    .and_then(valid_legacy_chat_key_str)
                {
                    source_chats.insert(source);
                }
            }
        }
        let resolved_name_only = if claimed_user_id.is_empty()
            && numeric_file_user.is_none()
            && file_name.starts_with("_n_")
        {
            resolve_name_only_backup_user_id(
                identity_source_path,
                &name,
                &source_chats.into_iter().collect::<Vec<_>>(),
            )?
        } else {
            None
        };
        let user_id = if !claimed_user_id.is_empty() {
            valid_legacy_user_id(&claimed_user_id).then_some(claimed_user_id)
        } else {
            numeric_file_user.or(resolved_name_only.clone())
        };
        let Some(user_id) = user_id else {
            summary.archived_only_files += 1;
            continue;
        };
        if resolved_name_only.is_some() {
            summary.resolved_name_only_files += 1;
        }
        let mut person_rows = Vec::new();
        let mut source_mapping_incomplete = false;
        for impression in impressions {
            let content_value = match impression {
                Value::String(content) => Some(content.as_str()),
                Value::Object(item) => item.get("content").and_then(Value::as_str),
                _ => None,
            };
            let content = content_value
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let content = content.chars().take(300).collect::<String>();
            if content.is_empty() {
                continue;
            }
            let mut sources = Vec::new();
            if let Some(item) = impression.as_object() {
                if let Some(chat) = item
                    .get("sourceChatKey")
                    .and_then(Value::as_str)
                    .filter(|chat| valid_legacy_chat_key_str(chat).is_some())
                {
                    sources.push(chat.to_owned());
                }
                sources.extend(
                    item.get("sourceChatKeys")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .filter(|chat| valid_legacy_chat_key_str(chat).is_some())
                        .map(str::to_owned),
                );
            }
            if sources.is_empty() {
                if impression.is_string() {
                    sources.extend(member_sources.iter().cloned());
                }
            }
            sources.sort();
            sources.dedup();
            if sources.is_empty() {
                source_mapping_incomplete = true;
                continue;
            }
            summary.source_impressions += 1;
            let item = impression.as_object();
            let created_at = json_timestamp(item.and_then(|item| item.get("createdAt")))?;
            let created_at = if created_at == 0 {
                now.max(0)
            } else {
                created_at
            };
            let last_observed_at =
                json_timestamp(item.and_then(|item| item.get("lastObservedAt")))?.max(created_at);
            let origin = item
                .and_then(|item| item.get("origin"))
                .and_then(Value::as_str)
                .filter(|origin| matches!(*origin, "model" | "consolidated" | "manual"))
                .unwrap_or_default();
            for chat_key in sources {
                person_rows.push(serde_json::json!({
                    "uin": user_id,
                    "name": name,
                    "chat_key": chat_key,
                    "content": content,
                    "created_at": created_at,
                    "observed_at": last_observed_at,
                    "origin": origin,
                    "source_file": format!("legacy/memory/people/{file_name}")
                }));
            }
        }
        if source_mapping_incomplete {
            summary.archived_only_files += 1;
        } else {
            rows.extend(person_rows);
        }
    }
    if summary.source_files == 0 {
        return Ok(summary);
    }
    if rows.is_empty() {
        return Ok(summary);
    }
    let mut identity = IdentityStore::open(target_path).map_err(|_| ImportError::ImportFailed)?;
    let imported = identity
        .import_legacy_global_memories(&rows, now)
        .map_err(|_| ImportError::ImportFailed)?
        .ok_or(ImportError::ImportFailed)?;
    if imported != rows.len() as u64 {
        return Err(ImportError::ImportFailed);
    }
    let mut expected = std::collections::BTreeMap::new();
    for row in &rows {
        let user_id = row["uin"].as_str().ok_or(ImportError::ImportFailed)?;
        let chat_key = row["chat_key"].as_str().ok_or(ImportError::ImportFailed)?;
        let content = row["content"].as_str().ok_or(ImportError::ImportFailed)?;
        let observed_at = row["observed_at"]
            .as_i64()
            .ok_or(ImportError::ImportFailed)?;
        let source_file = row["source_file"]
            .as_str()
            .ok_or(ImportError::ImportFailed)?;
        let created_at = row["created_at"]
            .as_i64()
            .ok_or(ImportError::ImportFailed)?;
        let origin = row["origin"].as_str().ok_or(ImportError::ImportFailed)?;
        expected
            .entry((user_id.to_owned(), chat_key.to_owned(), content.to_owned()))
            .or_insert((
                observed_at,
                source_file.to_owned(),
                created_at,
                origin.to_owned(),
            ));
    }
    let connection = Connection::open_with_flags(
        target_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| ImportError::ImportFailed)?;
    for ((user_id, chat_key, content), (observed_at, source_file, created_at, origin)) in &expected
    {
        let actual = connection
            .query_row(
                "SELECT observed_at,source_file,created_at,origin FROM legacy_memory_refs
             WHERE uin=?1 AND chat_key=?2 AND content=?3",
                params![user_id, chat_key, content],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| ImportError::ImportFailed)?;
        if actual.as_ref()
            != Some(&(
                *observed_at,
                source_file.clone(),
                *created_at,
                origin.clone(),
            ))
        {
            return Err(ImportError::ImportFailed);
        }
        summary.verified_rows += 1;
    }
    summary.target_unique_rows = expected.len() as u64;
    summary.verified_samples = summary.verified_rows.min(3);
    summary.imported_chat_scoped_rows = imported;
    drop(connection);
    drop(identity);
    verify_identity_database(target_path)?;
    restrict_file_permissions(target_path).map_err(|_| ImportError::ImportFailed)?;
    Ok(summary)
}

fn verify_identity_database(path: &Path) -> Result<(), ImportError> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| ImportError::ImportFailed)?;
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|_| ImportError::ImportFailed)?;
    let foreign_keys: i64 = connection
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .map_err(|_| ImportError::ImportFailed)?;
    if integrity != "ok" || foreign_keys != 0 {
        return Err(ImportError::ImportFailed);
    }
    Ok(())
}

fn import_memory_consolidation_backup_archive(
    backup_dir: &Path,
    identity_path: &Path,
    identity_source_path: &Path,
    imported_at: i64,
) -> Result<MemoryConsolidationBackupArchiveSummary, ImportError> {
    let mut summary = MemoryConsolidationBackupArchiveSummary::default();
    if !backup_dir.exists() {
        summary.available_to_rust_restore_ui = true;
        return Ok(summary);
    }
    let root = fs::symlink_metadata(backup_dir).map_err(|_| ImportError::ImportFailed)?;
    if root.file_type().is_symlink() || !root.is_dir() {
        return Err(ImportError::ImportFailed);
    }
    summary.archive_present = true;
    summary.preserved_in_legacy_archive = true;
    // Preserve every Node snapshot in the byte-verified legacy tree. Typed
    // restore rows are imported only for exact QQ identities or a unique
    // name/alias match scoped to the snapshot's source conversations.
    let mut import_rows = Vec::new();
    for person_dir in fs::read_dir(backup_dir).map_err(|_| ImportError::ImportFailed)? {
        let person_dir = person_dir.map_err(|_| ImportError::ImportFailed)?;
        let kind = person_dir
            .file_type()
            .map_err(|_| ImportError::ImportFailed)?;
        if kind.is_symlink() || !kind.is_dir() {
            return Err(ImportError::ImportFailed);
        }
        for backup in fs::read_dir(person_dir.path()).map_err(|_| ImportError::ImportFailed)? {
            let backup = backup.map_err(|_| ImportError::ImportFailed)?;
            let kind = backup.file_type().map_err(|_| ImportError::ImportFailed)?;
            let name = backup.file_name();
            let name = name.to_str().ok_or(ImportError::ImportFailed)?;
            if !name.ends_with(".json") {
                return Err(ImportError::ImportFailed);
            }
            if kind.is_symlink() || !kind.is_file() {
                return Err(ImportError::ImportFailed);
            }
            let value = read_import_json(&backup.path())?;
            if !is_importable_memory_consolidation_backup(&value) {
                return Err(ImportError::ImportFailed);
            }
            let object = value.as_object().ok_or(ImportError::ImportFailed)?;
            let person = object
                .get("person")
                .and_then(Value::as_object)
                .ok_or(ImportError::ImportFailed)?;
            let backed_up_at = json_timestamp(object.get("backedUpAt"))?;
            let reason = object
                .get("reason")
                .and_then(Value::as_str)
                .ok_or(ImportError::ImportFailed)?;
            let source_chat_key = object
                .get("sourceChatKey")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !source_chat_key.is_empty() && valid_legacy_chat_key_str(source_chat_key).is_none() {
                return Err(ImportError::ImportFailed);
            }
            let impressions = person
                .get("impressions")
                .and_then(Value::as_array)
                .ok_or(ImportError::ImportFailed)?;
            let directory_user_name = person_dir.file_name();
            let directory_user = directory_user_name
                .to_str()
                .ok_or(ImportError::ImportFailed)?;
            let person_user = person
                .get("userId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let resolved_name_only = if !valid_legacy_user_id(directory_user)
                && directory_user.starts_with("_n_")
                && person_user.is_empty()
            {
                let name = person
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let mut source_chats = std::collections::BTreeSet::new();
                if let Some(source) = valid_legacy_chat_key_str(source_chat_key) {
                    source_chats.insert(source);
                }
                source_chats.extend(
                    person
                        .get("sourceChatKeys")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .filter_map(valid_legacy_chat_key_str),
                );
                for impression in impressions {
                    source_chats.extend(
                        impression
                            .get("sourceChatKeys")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .filter_map(valid_legacy_chat_key_str),
                    );
                }
                resolve_name_only_backup_user_id(
                    identity_source_path,
                    name,
                    &source_chats.into_iter().collect::<Vec<_>>(),
                )?
            } else {
                None
            };
            let resolved_user_id =
                if valid_legacy_user_id(directory_user) && person_user == directory_user {
                    Some(directory_user.to_owned())
                } else {
                    resolved_name_only.clone()
                };
            let source_provenance_valid = impressions.iter().all(|impression| {
                let Some(entry) = impression.as_object() else {
                    return false;
                };
                let sources = entry.get("sourceChatKeys").and_then(Value::as_array);
                match sources {
                    Some(sources) if !sources.is_empty() => sources.iter().all(|source| {
                        source
                            .as_str()
                            .is_some_and(|value| valid_legacy_chat_key_str(value).is_some())
                    }),
                    _ => valid_legacy_chat_key_str(source_chat_key).is_some(),
                }
            });
            if let Some(user_id) = resolved_user_id.filter(|_| source_provenance_valid) {
                if resolved_name_only.is_some() {
                    summary.resolved_name_only_files += 1;
                }
                summary.restorable_files += 1;
                let mut restorable_person = Value::Object(person.clone());
                if resolved_name_only.is_some() {
                    restorable_person["userId"] = Value::String(user_id.clone());
                }
                import_rows.push(serde_json::json!({
                    "uin": user_id,
                    "backup_key": format!("{user_id}/{name}"),
                    "backed_up_at": backed_up_at,
                    "reason": reason,
                    "source_chat_key": source_chat_key,
                    "person": restorable_person,
                }));
            } else {
                summary.archived_only_files += 1;
            }
            summary.source_files += 1;
            summary.verified_files += 1;
            summary.person_snapshots += 1;
            summary.impressions += impressions.len() as u64;
        }
    }
    if !import_rows.is_empty() {
        let mut identity =
            IdentityStore::open(identity_path).map_err(|_| ImportError::ImportFailed)?;
        summary.imported_restorable_files = identity
            .import_node_memory_consolidation_backups(&import_rows, imported_at)
            .map_err(|_| ImportError::ImportFailed)?
            .ok_or(ImportError::ImportFailed)?;
        if summary.imported_restorable_files != import_rows.len() as u64 {
            return Err(ImportError::ImportFailed);
        }
    }
    summary.available_to_rust_restore_ui =
        summary.imported_restorable_files == summary.source_files;
    Ok(summary)
}

fn resolve_name_only_backup_user_id(
    identity_source_path: &Path,
    name: &str,
    source_chat_keys: &[String],
) -> Result<Option<String>, ImportError> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() || !identity_source_path.is_file() {
        return Ok(None);
    }
    let source = Connection::open_with_flags(
        identity_source_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| ImportError::ImportFailed)?;
    let has_people: bool = source
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='people')",
            [],
            |row| row.get(0),
        )
        .map_err(|_| ImportError::ImportFailed)?;
    if !has_people {
        return Ok(None);
    }
    let has_sources: bool = source
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='identity_sources')",
            [],
            |row| row.get(0),
        )
        .map_err(|_| ImportError::ImportFailed)?;
    if !has_sources {
        return Ok(None);
    }
    let has_aliases: bool = source
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='identity_aliases')",
            [],
            |row| row.get(0),
        )
        .map_err(|_| ImportError::ImportFailed)?;
    let scopes = if source_chat_keys.is_empty() {
        vec![String::new()]
    } else {
        source_chat_keys.to_vec()
    };
    let mut candidates = std::collections::BTreeSet::new();
    let mut people = source
        .prepare(
            "SELECT p.uin FROM people p
             WHERE p.primary_name=?1
               AND (?2='' OR EXISTS(
                   SELECT 1 FROM identity_sources s WHERE s.uin=p.uin AND s.chat_key=?2
               ))",
        )
        .map_err(|_| ImportError::ImportFailed)?;
    for scope in &scopes {
        candidates.extend(
            people
                .query_map(rusqlite::params![&name, scope], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|_| ImportError::ImportFailed)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| ImportError::ImportFailed)?,
        );
    }
    if has_aliases {
        let mut aliases = source
            .prepare(
                "SELECT DISTINCT uin FROM identity_aliases
                 WHERE alias=?1 AND (?2='' OR chat_key=?2)",
            )
            .map_err(|_| ImportError::ImportFailed)?;
        for scope in &scopes {
            candidates.extend(
                aliases
                    .query_map(rusqlite::params![&name, scope], |row| {
                        row.get::<_, String>(0)
                    })
                    .map_err(|_| ImportError::ImportFailed)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| ImportError::ImportFailed)?,
            );
        }
    }
    candidates.retain(|user_id| valid_legacy_user_id(user_id));
    if candidates.len() == 1 {
        Ok(candidates.into_iter().next())
    } else {
        Ok(None)
    }
}

fn import_memory_consolidation_cooldowns(
    memory_dir: &Path,
    identity_source_path: &Path,
    target_path: &Path,
    enabled: bool,
) -> Result<MemoryConsolidationCooldownImportSummary, ImportError> {
    let mut summary = MemoryConsolidationCooldownImportSummary::default();
    if !enabled || !memory_dir.exists() {
        return Ok(summary);
    }
    let root_kind = fs::symlink_metadata(memory_dir).map_err(|_| ImportError::ImportFailed)?;
    if root_kind.file_type().is_symlink() || !root_kind.is_dir() {
        return Err(ImportError::ImportFailed);
    }

    let mut cooldowns = std::collections::BTreeMap::<String, i64>::new();
    let mut person_cooldowns = std::collections::BTreeMap::<String, i64>::new();
    let mut person_updated_at = std::collections::BTreeMap::<String, i64>::new();
    for entry in fs::read_dir(memory_dir).map_err(|_| ImportError::ImportFailed)? {
        let entry = entry.map_err(|_| ImportError::ImportFailed)?;
        let kind = entry.file_type().map_err(|_| ImportError::ImportFailed)?;
        let name = entry.file_name();
        let name = name.to_str().ok_or(ImportError::ImportFailed)?;
        if name == "people" {
            if kind.is_symlink() || !kind.is_dir() {
                return Err(ImportError::ImportFailed);
            }
            for person in fs::read_dir(entry.path()).map_err(|_| ImportError::ImportFailed)? {
                let person = person.map_err(|_| ImportError::ImportFailed)?;
                let person_kind = person.file_type().map_err(|_| ImportError::ImportFailed)?;
                let file_name = person.file_name();
                let file_name = file_name.to_str().ok_or(ImportError::ImportFailed)?;
                if !file_name.ends_with(".json") {
                    continue;
                }
                if person_kind.is_symlink() || !person_kind.is_file() {
                    return Err(ImportError::ImportFailed);
                }
                summary.source_person_files += 1;
                let value = read_import_json(&person.path())?;
                let Some(object) = value.as_object() else {
                    return Err(ImportError::ImportFailed);
                };
                let Some(impressions) = object.get("impressions").and_then(Value::as_array) else {
                    continue;
                };
                if impressions.is_empty() {
                    continue;
                }
                let mut chats = std::collections::BTreeSet::new();
                if let Some(keys) = object.get("sourceChatKeys").and_then(Value::as_array) {
                    chats.extend(keys.iter().filter_map(valid_legacy_chat_key));
                }
                for impression in impressions {
                    let Some(impression) = impression.as_object() else {
                        continue;
                    };
                    if let Some(key) = impression
                        .get("sourceChatKey")
                        .and_then(Value::as_str)
                        .and_then(valid_legacy_chat_key_str)
                    {
                        chats.insert(key);
                    }
                    if let Some(keys) = impression.get("sourceChatKeys").and_then(Value::as_array) {
                        chats.extend(keys.iter().filter_map(valid_legacy_chat_key));
                    }
                }
                let mut user_id = object
                    .get("userId")
                    .and_then(Value::as_str)
                    .filter(|user_id| valid_legacy_user_id(user_id))
                    .or_else(|| {
                        file_name
                            .strip_suffix(".json")
                            .filter(|user_id| valid_legacy_user_id(user_id))
                    })
                    .map(str::to_owned);
                if user_id.is_none()
                    && file_name.starts_with("_n_")
                    && identity_source_path.is_file()
                {
                    let sources = chats.iter().cloned().collect::<Vec<_>>();
                    let name = object
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    user_id =
                        resolve_name_only_backup_user_id(identity_source_path, name, &sources)?;
                }
                if let Some(user_id) = &user_id {
                    let updated_at = json_timestamp(object.get("updatedAt"))?;
                    person_updated_at
                        .entry(user_id.clone())
                        .and_modify(|at| *at = (*at).max(updated_at))
                        .or_insert(updated_at);
                }
                let timestamp = json_timestamp(object.get("lastConsolidatedAt"))?;
                if timestamp > 0 {
                    if let Some(user_id) = user_id {
                        person_cooldowns
                            .entry(user_id.clone())
                            .and_modify(|at| *at = (*at).max(timestamp))
                            .or_insert(timestamp);
                    }
                }
                for chat in chats {
                    cooldowns
                        .entry(chat)
                        .and_modify(|at| *at = (*at).max(timestamp))
                        .or_insert(timestamp);
                }
            }
            continue;
        }
        let Some(chat_key) = valid_legacy_chat_directory(name) else {
            if kind.is_symlink() {
                return Err(ImportError::ImportFailed);
            }
            continue;
        };
        if kind.is_symlink() || !kind.is_dir() {
            return Err(ImportError::ImportFailed);
        }
        for child in fs::read_dir(entry.path()).map_err(|_| ImportError::ImportFailed)? {
            let child = child.map_err(|_| ImportError::ImportFailed)?;
            let child_kind = child.file_type().map_err(|_| ImportError::ImportFailed)?;
            let child_name = child.file_name();
            let child_name = child_name.to_str().ok_or(ImportError::ImportFailed)?;
            if child_name == "_meta.json" {
                if child_kind.is_symlink() || !child_kind.is_file() {
                    return Err(ImportError::ImportFailed);
                }
                summary.source_chat_metadata_files += 1;
                let value = read_import_json(&child.path())?;
                let object = value.as_object().ok_or(ImportError::ImportFailed)?;
                let timestamp = json_timestamp(object.get("lastConsolidatedAt"))?;
                cooldowns
                    .entry(chat_key.clone())
                    .and_modify(|at| *at = (*at).max(timestamp))
                    .or_insert(timestamp);
            } else if !child_name.starts_with('_') && child_name.ends_with(".json") {
                if child_kind.is_symlink() || !child_kind.is_file() {
                    return Err(ImportError::ImportFailed);
                }
                summary.source_person_files += 1;
                let value = read_import_json(&child.path())?;
                let Some(object) = value.as_object() else {
                    return Err(ImportError::ImportFailed);
                };
                if object
                    .get("impressions")
                    .and_then(Value::as_array)
                    .is_some_and(|impressions| !impressions.is_empty())
                {
                    let timestamp = json_timestamp(object.get("lastConsolidatedAt"))?;
                    cooldowns
                        .entry(chat_key.clone())
                        .and_modify(|at| *at = (*at).max(timestamp))
                        .or_insert(timestamp);
                }
            }
        }
    }

    let rows = cooldowns
        .into_iter()
        .filter(|(_, timestamp)| *timestamp > 0)
        .collect::<Vec<_>>();
    let people_rows = person_cooldowns.into_iter().collect::<Vec<_>>();
    let updated_rows = person_updated_at.into_iter().collect::<Vec<_>>();
    if rows.is_empty() && people_rows.is_empty() && updated_rows.is_empty() {
        return Ok(summary);
    }
    let mut store =
        MemoryConsolidationStore::open(target_path).map_err(|_| ImportError::ImportFailed)?;
    store
        .import_chat_cooldowns(&rows)
        .map_err(|_| ImportError::ImportFailed)?;
    store
        .import_person_cooldowns(&people_rows)
        .map_err(|_| ImportError::ImportFailed)?;
    store
        .import_person_updated_at(&updated_rows)
        .map_err(|_| ImportError::ImportFailed)?;
    if store
        .cooldown_count()
        .map_err(|_| ImportError::ImportFailed)?
        != rows.len() as u64
        || !store
            .integrity_ok()
            .map_err(|_| ImportError::ImportFailed)?
    {
        return Err(ImportError::ImportFailed);
    }
    for (chat, timestamp) in &rows {
        let state = store
            .chat_run_state(chat)
            .map_err(|_| ImportError::ImportFailed)?
            .ok_or(ImportError::ImportFailed)?;
        if state.last_attempt_at != *timestamp
            || state.last_finished_at != *timestamp
            || state.status != "completed"
        {
            return Err(ImportError::ImportFailed);
        }
        summary.verified_chat_rows += 1;
    }
    for (user_id, timestamp) in &people_rows {
        if store
            .latest_person_cooldown(std::slice::from_ref(user_id))
            .map_err(|_| ImportError::ImportFailed)?
            != *timestamp
        {
            return Err(ImportError::ImportFailed);
        }
        summary.verified_person_cooldown_rows += 1;
    }
    for (user_id, timestamp) in &updated_rows {
        if store
            .person_updated_at(user_id)
            .map_err(|_| ImportError::ImportFailed)?
            != *timestamp
        {
            return Err(ImportError::ImportFailed);
        }
        summary.verified_person_updated_rows += 1;
    }
    summary.verified_samples = summary.verified_chat_rows.min(3);
    summary.imported_chat_cooldowns = rows.len() as u64;
    summary.imported_person_cooldowns = people_rows.len() as u64;
    summary.imported_person_updated_rows = updated_rows.len() as u64;
    Ok(summary)
}

fn read_import_json(path: &Path) -> Result<Value, ImportError> {
    let bytes = fs::read(path).map_err(|_| ImportError::ImportFailed)?;
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    serde_json::from_slice(&normalize_node_json(bytes)).map_err(|_| ImportError::ImportFailed)
}

pub(crate) fn is_importable_memory_consolidation_backup(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.get("version").and_then(Value::as_u64) != Some(1)
        || object
            .get("reason")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        || !json_timestamp(object.get("backedUpAt")).is_ok_and(|timestamp| timestamp > 0)
    {
        return false;
    }
    if object
        .get("sourceChatKey")
        .and_then(Value::as_str)
        .is_some_and(|chat_key| {
            !chat_key.is_empty() && valid_legacy_chat_key_str(chat_key).is_none()
        })
    {
        return false;
    }
    let Some(person) = object.get("person").and_then(Value::as_object) else {
        return false;
    };
    let Some(impressions) = person.get("impressions").and_then(Value::as_array) else {
        return false;
    };
    impressions.iter().all(|impression| {
        impression
            .as_str()
            .or_else(|| impression.get("content").and_then(Value::as_str))
            .is_some_and(|content| !content.trim().is_empty())
    })
}

pub(crate) fn is_importable_person_memory_record(person: &Value) -> bool {
    person
        .as_object()
        .and_then(|object| object.get("impressions"))
        .and_then(Value::as_array)
        .is_some()
}

fn json_timestamp(value: Option<&Value>) -> Result<i64, ImportError> {
    let Some(value) = value else {
        return Ok(0);
    };
    let timestamp = value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|number| i64::try_from(number).ok()))
        .or_else(|| value.as_str().and_then(|text| text.parse::<i64>().ok()))
        .ok_or(ImportError::ImportFailed)?;
    if timestamp < 0 {
        return Err(ImportError::ImportFailed);
    }
    Ok(timestamp)
}

fn valid_legacy_chat_directory(name: &str) -> Option<String> {
    let (kind, id) = name.split_once('_')?;
    if !matches!(kind, "group" | "private")
        || id.is_empty()
        || id.len() > 15
        || !id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some(format!("{kind}:{id}"))
}

fn valid_legacy_chat_key(value: &Value) -> Option<String> {
    valid_legacy_chat_key_str(value.as_str()?)
}

fn valid_legacy_chat_key_str(value: &str) -> Option<String> {
    let (kind, id) = value.split_once(':')?;
    if !matches!(kind, "group" | "private")
        || id.is_empty()
        || id.len() > 15
        || !id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some(value.to_owned())
}

fn valid_legacy_user_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 15 && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn import_core_runtime_config(source: &Path, destination: &Path) -> ConfigurationImportReview {
    let raw = fs::read(source).ok();
    let parsed = raw
        .as_deref()
        .map(normalize_node_json)
        .as_deref()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(bytes).ok());
    let enabled = parsed
        .as_ref()
        .map(effective_node_enabled_features)
        .unwrap_or_default();
    let unmapped = parsed
        .as_ref()
        .map(|config| unmigrated_node_features(config, &enabled))
        .unwrap_or_else(|| enabled.clone());
    let mapped = parsed
        .as_ref()
        .and_then(map_core_runtime_config)
        .map(|mut config| {
            config.unmigrated_enabled_features = Some(unmapped.clone());
            config
        })
        .filter(|config| {
            config
                .onebot_ws_url
                .as_deref()
                .is_some_and(|v| !v.is_empty())
                && config
                    .model_base_url
                    .as_deref()
                    .is_some_and(|v| !v.is_empty())
                && config.model_id.as_deref().is_some_and(|v| !v.is_empty())
                && (config
                    .allow_groups
                    .as_deref()
                    .is_some_and(|v| !v.is_empty())
                    || config
                        .allow_private
                        .as_deref()
                        .is_some_and(|v| !v.is_empty()))
                && config
                    .model_api_key
                    .as_deref()
                    .is_some_and(|v| !v.trim().is_empty())
        });
    let core_mapped = if let Some(config) = mapped {
        if let Ok(serialized) = serde_json::to_vec_pretty(&config) {
            if private_new_file(destination)
                .and_then(|mut output| output.write_all(&serialized))
                .is_ok()
            {
                true
            } else {
                false
            }
        } else {
            false
        }
    } else {
        false
    };
    ConfigurationImportReview {
        legacy_config_present: raw.is_some(),
        legacy_config_valid_json: parsed.is_some(),
        secrets_exposed_in_report: false,
        core_runtime_configuration_mapped: core_mapped,
        enabled_features: enabled,
        unmapped_enabled_features: unmapped,
        enabled_pilot_data_missing: Vec::new(),
        enabled_feature_data_missing: Vec::new(),
    }
}

/// The current Rust web tools implement the active Node Bing configuration.
/// Keep alternate providers/custom endpoints as active-mode blockers rather
/// than silently importing them as if their behavior were equivalent.
pub(crate) fn unmigrated_node_features(
    config: &serde_json::Value,
    enabled: &[String],
) -> Vec<String> {
    enabled
        .iter()
        .filter(|feature| match feature.as_str() {
            // Incoming friend requests have a dedicated Rust ledger, review
            // API/UI, OneBot decision executor and archived-history importer.
            // Its independent settings are mapped by map_core_runtime_config.
            "incomingFriendRequest" => false,
            // Chat-scoped handoff has a durable Rust store, model/UI/API
            // lifecycle, bounded Node JSON importer and mapped runtime policy.
            "memoryHandoff" => false,
            // The scheduler, durable wake ledger, bounded `schedule_wake`
            // tool, follow-up trigger, active-window deferral, lifecycle API,
            // and dedicated management page are implemented and covered by
            // Node/Rust behavior tests. Runtime activation is still guarded
            // separately by the imported feature/data audits and ledger.
            "proactive" | "followUp" | "selfWake" => false,
            // Group requests and direct bot entries have dedicated OneBot
            // normalization, durable decision/notification ledgers, mapped
            // runtime controls, whitelist compensation, and a management
            // page. Real NapCat/QQ acceptance remains a separate cutover gate.
            "groupInvite" => false,
            // Incident capture/deduplication, lifecycle transitions, per-chat
            // controls, bounded retention, authenticated operations, and
            // administrator notifications all have Rust-owned persistence
            // and management UI. Legacy incidents and controls are imported
            // with per-row readback; real QQ alert delivery remains a separate
            // cutover acceptance gate.
            "incidentPilot" => false,
            // Node applies the global token-saver cap to daily-moment model
            // rounds at every run. Rust does not yet preserve that dynamic
            // policy, so do not claim parity while a capped mode is active.
            "dailyMoments" => !node_daily_moments_is_rust_compatible(config),
            // QZone prompts, provider-default output limits, input-budget
            // clipping, durable archive state, and write receipts are mapped.
            // Keep the feature blocked if Node has request-body/thinking
            // overrides the Rust primary model adapter cannot reproduce, or
            // persona fields that the imported projection would truncate.
            "qzoneInteractions" => !node_qzone_interactions_is_rust_compatible(config),
            // The sticker library, prompt/tool integration, collection
            // ledger, asset importer, capacity handling, and dedicated
            // management pages are implemented in Rust. Live NapCat/QQ
            // behavior remains a separate cutover acceptance gate.
            "sticker" | "stickerCollect" => false,
            // Relationship evaluation is shadow-only, has a dedicated
            // feature-owned ledger, independently persisted lifecycle
            // settings, bounded evidence/model prompts, authenticated
            // archive/trace APIs, and verified legacy-table import. Real
            // model/NapCat acceptance is enforced by separate cutover gates.
            "relationshipPilot" => false,
            // Memory consolidation has matching automatic/manual execution,
            // global provenance revisions, source-scoped restore, and bounded
            // legacy-data gates when it follows the shared chat model. Node's
            // separate memory provider/model override is not implemented.
            "memoryConsolidate" => {
                config
                    .pointer("/memory/useChatModel")
                    .and_then(serde_json::Value::as_bool)
                    == Some(false)
            }
            // The base identity index/profile/merge path is implemented in
            // Rust. Node's separate active friend-proposal experiment is not;
            // keep the parent gate closed if that subfeature is enabled.
            "identityPilot" => config
                .pointer("/identityPilot/friendProposal/enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            // Rust does not yet implement Node's deferred paced-conversation
            // mode or its experimental parallel read-wave scheduler.
            "pacing" | "toolSchedulerPilot" => true,
            // The recall notification has a dedicated Rust executor, routes
            // all writes through the durable outbox, imports legacy sent
            // markers, and quarantines unknown writes without retrying.
            "recallNotifications" => !config
                .pointer("/admin/ownerUin")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .is_some_and(|owner| {
                    (5..=15).contains(&owner.len())
                        && owner.bytes().all(|byte| byte.is_ascii_digit())
                }),
            "visionInput" | "visionFallback" => !node_vision_path_is_rust_compatible(config),
            "webSearch" => !node_web_search_is_rust_compatible(config),
            "modelFallback" => !node_model_fallback_is_rust_compatible(config),
            // Rust persists inspected image/sticker source references through
            // the chat-scoped handoff used by legacy mode. Node's additional
            // provider-transcript rewrite is only exercised by lifecycle mode,
            // which Rust does not implement and must remain a cutover blocker.
            "multimodalContext" => node_multimodal_context_requires_lifecycle(config),
            _ => true,
        })
        .cloned()
        .collect()
}

fn node_daily_moments_is_rust_compatible(config: &serde_json::Value) -> bool {
    !config
        .pointer("/tokenSaver/mode")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|mode| {
            matches!(
                mode.trim().to_ascii_lowercase().as_str(),
                "balanced" | "aggressive"
            )
        })
}

fn node_qzone_interactions_is_rust_compatible(config: &serde_json::Value) -> bool {
    let api = config.get("api");
    let extra_body = api.and_then(|api| api.get("extraBody"));
    if extra_body.is_some_and(|value| {
        !value.is_null() && value.as_object().is_none_or(|body| !body.is_empty())
    }) {
        return false;
    }

    if node_qzone_write_intent(config) != "on" && node_qzone_thinking_patch(config).is_none() {
        return false;
    }

    let persona = config.get("persona");
    [
        ("botName", 100),
        ("selfNickname", 100),
        ("selfAppearance", 4_000),
        ("roleText", 20_000),
        ("customRules", 8_000),
    ]
    .into_iter()
    .all(|(field, limit)| {
        persona
            .and_then(|persona| persona.get(field))
            .and_then(serde_json::Value::as_str)
            .is_none_or(|value| value.encode_utf16().count() <= limit)
    })
}

fn node_qzone_write_intent(config: &serde_json::Value) -> String {
    let api = config.get("api");
    let host = api
        .and_then(|api| api.get("baseUrl"))
        .and_then(serde_json::Value::as_str)
        .and_then(|url| reqwest::Url::parse(url.trim()).ok())
        .and_then(|url| url.host_str().map(|host| host.to_ascii_lowercase()));
    let raw = api.and_then(|api| {
        let by_service = api
            .get("thinkingByService")
            .and_then(serde_json::Value::as_object);
        host.as_ref()
            .and_then(|host| by_service.and_then(|values| values.get(host)))
            .or_else(|| api.get("thinking"))
    });
    let value = match raw {
        Some(serde_json::Value::Object(values)) => {
            values.get("write").or_else(|| values.get("default"))
        }
        value => value,
    };
    match value {
        Some(serde_json::Value::Bool(false)) => "off".into(),
        Some(serde_json::Value::String(value))
            if matches!(
                value.to_ascii_lowercase().as_str(),
                "off" | "low" | "medium" | "high" | "max"
            ) =>
        {
            value.to_ascii_lowercase()
        }
        _ => "on".into(),
    }
}

fn node_qzone_thinking_patch(config: &serde_json::Value) -> Option<serde_json::Value> {
    let intent = node_qzone_write_intent(config);
    if intent == "on" {
        return None;
    }
    let api = config.get("api")?;
    let provider_id = api.get("provider").and_then(serde_json::Value::as_str)?;
    let provider_is_custom = config
        .get("providers")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .find(|provider| {
            provider.get("id").and_then(serde_json::Value::as_str) == Some(provider_id)
        })
        .and_then(|provider| provider.get("preset"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|preset| preset.eq_ignore_ascii_case("custom"));
    if !provider_is_custom {
        return None;
    }
    let patch = api
        .get("thinkingParams")
        .and_then(|params| params.get(&intent))
        .and_then(serde_json::Value::as_object)
        .cloned()
        .map(serde_json::Value::Object)
        .or_else(|| {
            (intent == "off").then(|| serde_json::json!({"thinking":{"type":"disabled"}}))
        })?;
    let object = patch.as_object()?;
    if object.len() != 1
        || object
            .get("thinking")
            .and_then(serde_json::Value::as_object)
            .is_none()
        || serde_json::to_vec(&patch).map_or(true, |bytes| bytes.len() > 4_096)
    {
        return None;
    }
    Some(patch)
}

fn node_multimodal_context_requires_lifecycle(config: &serde_json::Value) -> bool {
    let Some(conversation) = config
        .get("conversation")
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    if conversation.get("mode").and_then(serde_json::Value::as_str) == Some("lifecycle") {
        return true;
    }
    conversation
        .get("unifiedMode")
        .and_then(serde_json::Value::as_bool)
        == Some(false)
        && conversation
            .get("groupModes")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|groups| {
                groups
                    .values()
                    .any(|mode| mode.as_str() == Some("lifecycle"))
            })
}

/// Rust routes image understanding through the imported dedicated vision
/// model. Only treat Node image input/fallback as mapped when that exact route
/// is enabled, has a safe endpoint/model, and has an importable credential.
fn node_vision_path_is_rust_compatible(config: &serde_json::Value) -> bool {
    let Some(api) = config.get("api").and_then(serde_json::Value::as_object) else {
        return false;
    };
    if api.get("vision").and_then(serde_json::Value::as_bool) == Some(false) {
        return false;
    }
    let Some(vision) = api
        .get("visionModel")
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    if vision.get("enabled").and_then(serde_json::Value::as_bool) == Some(false) {
        return false;
    }
    let model = vision
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim();
    if model.is_empty() || model.chars().count() > 200 {
        return false;
    }
    // Node's newly deployed pool can activate later without another config
    // write. Rust currently imports only one fixed vision model, so a future
    // balanceFrom date is not a reason to silently discard the other models.
    let mut vision_models = std::collections::BTreeSet::from([model]);
    if let Some(models) = vision.get("models").and_then(serde_json::Value::as_array) {
        for candidate in models.iter().filter_map(serde_json::Value::as_str) {
            let candidate = candidate.trim();
            if !candidate.is_empty() {
                vision_models.insert(candidate);
            }
        }
    }
    if vision_models.len() > 1 {
        return false;
    }
    // The Rust vision request does not yet emit provider-specific thinking
    // controls. Do not call an explicit Node on/off override equivalent to
    // an omitted request parameter (especially for Pro reasoning models).
    if vision
        .get("thinking")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| !matches!(value.trim(), "" | "auto"))
    {
        return false;
    }
    let provider = vision
        .get("provider")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim();
    let provider_entry = config
        .get("providers")
        .and_then(serde_json::Value::as_array)
        .and_then(|providers| {
            providers
                .iter()
                .find(|entry| entry.get("id").and_then(serde_json::Value::as_str) == Some(provider))
        });
    let base_url = vision
        .get("baseUrl")
        .and_then(serde_json::Value::as_str)
        .filter(|url| !url.trim().is_empty())
        .or_else(|| {
            provider_entry
                .and_then(|entry| entry.get("baseURL"))
                .and_then(serde_json::Value::as_str)
                .filter(|url| !url.trim().is_empty())
        });
    let Some(url) = base_url.and_then(|value| reqwest::Url::parse(value.trim()).ok()) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let explicit_key = vision
        .get("apiKey")
        .and_then(serde_json::Value::as_str)
        .filter(|key| !key.trim().is_empty() && *key != "******");
    let provider_key = config
        .get("providerKeys")
        .and_then(|keys| keys.get(provider))
        .and_then(serde_json::Value::as_str)
        .filter(|key| !key.trim().is_empty() && *key != "******");
    explicit_key.or(provider_key).is_some()
}

fn node_model_fallback_is_rust_compatible(config: &serde_json::Value) -> bool {
    let Some(api) = config.get("api").and_then(serde_json::Value::as_object) else {
        return false;
    };
    let Some(fallback) = api.get("fallback").and_then(serde_json::Value::as_object) else {
        return false;
    };
    let model = fallback
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if model.trim().is_empty()
        || fallback.get("enabled").and_then(serde_json::Value::as_bool) == Some(false)
    {
        return false;
    }
    let extra_body = api.get("extraBody");
    if extra_body.is_some_and(|value| {
        !value.is_null() && value.as_object().is_none_or(|body| !body.is_empty())
    }) {
        return false;
    }
    let url = fallback
        .get("baseUrl")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            api.get("baseUrl")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
        });
    let Some(url) = url.and_then(|value| reqwest::Url::parse(value.trim()).ok()) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    for value in [api.get("timeoutMs"), fallback.get("timeoutMs")]
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_f64)
    {
        if !value.is_finite() || value > 300_000.0 {
            return false;
        }
    }
    true
}

fn node_model_timeout_ms(value: Option<&serde_json::Value>) -> Option<u64> {
    let timeout_ms = value.and_then(serde_json::Value::as_f64)?;
    if !timeout_ms.is_finite() || timeout_ms == 0.0 {
        return None;
    }
    Some(timeout_ms.round().clamp(5_000.0, 300_000.0) as u64)
}

fn node_web_search_is_rust_compatible(config: &serde_json::Value) -> bool {
    let settings = config
        .get("webSearch")
        .and_then(serde_json::Value::as_object);
    let provider = settings
        .and_then(|settings| settings.get("provider"))
        .and_then(serde_json::Value::as_str)
        .filter(|provider| !provider.trim().is_empty())
        .unwrap_or("bing");
    if !provider.eq_ignore_ascii_case("bing") {
        return false;
    }
    let search_url = settings
        .and_then(|settings| settings.get("searchUrl"))
        .and_then(serde_json::Value::as_str)
        .filter(|url| !url.trim().is_empty())
        .unwrap_or("https://cn.bing.com/search");
    crate::application::web_tools::WebSearchSettings {
        enabled: true,
        provider: "bing".into(),
        search_url: search_url.trim().to_owned(),
        max_results: 6,
    }
    .validate()
    .is_ok()
}

fn map_core_runtime_config(value: &serde_json::Value) -> Option<ImportedNodeRuntimeConfig> {
    let object = value.as_object()?;
    let string = |path: &[&str]| -> Option<String> {
        path.iter()
            .try_fold(serde_json::Value::Object(object.clone()), |current, key| {
                current.get(*key).cloned()
            })
            .and_then(|value| value.as_str().map(str::to_owned))
    };
    let number = |path: &[&str]| -> Option<f64> {
        path.iter()
            .try_fold(serde_json::Value::Object(object.clone()), |current, key| {
                current.get(*key).cloned()
            })
            .and_then(|value| value.as_f64())
    };
    let boolean = |path: &[&str]| -> Option<bool> {
        path.iter()
            .try_fold(serde_json::Value::Object(object.clone()), |current, key| {
                current.get(*key).cloned()
            })
            .and_then(|value| value.as_bool())
    };
    let requested_handoff_chars = number(&["memory", "handoffMaxChars"])
        .filter(|value| value.is_finite() && *value != 0.0)
        .unwrap_or(4_000.0)
        .round()
        .clamp(0.0, 12_000.0) as usize;
    let handoff_cap = match string(&["tokenSaver", "mode"])
        .unwrap_or_else(|| "off".to_owned())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "balanced" => 2_000,
        "aggressive" => 1_200,
        _ => usize::MAX,
    };
    let memory_handoff_max_chars = requested_handoff_chars.min(handoff_cap).clamp(500, 12_000);
    let requested_sticker_prompt_max = number(&["sticker", "promptMaxStickers"])
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(10.0)
        .round()
        .clamp(1.0, 60.0) as usize;
    let sticker_prompt_max_cap = match string(&["tokenSaver", "mode"])
        .unwrap_or_else(|| "off".to_owned())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "balanced" => 5,
        "aggressive" => 3,
        _ => usize::MAX,
    };
    let sticker_prompt_max_stickers = requested_sticker_prompt_max.min(sticker_prompt_max_cap);
    let id_list = |path: &[&str]| -> Option<String> {
        let values = path
            .iter()
            .try_fold(serde_json::Value::Object(object.clone()), |current, key| {
                current.get(*key).cloned()
            })?;
        let values = values.as_array()?;
        Some(
            values
                .iter()
                .filter_map(|value| match value {
                    serde_json::Value::String(value) => Some(value.clone()),
                    serde_json::Value::Number(value) => Some(value.to_string()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(","),
        )
    };
    let keywords = string(&["store", "keywords"]).or_else(|| {
        object
            .get("store")?
            .get("keywords")?
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(",")
            })
    });
    let selected_provider = string(&["api", "provider"]).unwrap_or_default();
    let selected_provider_key = object
        .get("providerKeys")
        .and_then(|keys| keys.get(&selected_provider))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let blocklist = object
        .get("blocklist")
        .and_then(serde_json::Value::as_object)
        .map(|groups| {
            groups
                .iter()
                .filter_map(|(group_id, users)| {
                    let users = users.as_array()?;
                    let users = users
                        .iter()
                        .filter_map(|user| match user {
                            serde_json::Value::String(value) => Some(value.clone()),
                            serde_json::Value::Number(value) => Some(value.to_string()),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    Some((group_id.clone(), users))
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let slider_mode = string(&["store", "sliderMode"]).unwrap_or_default();
    let random_response_percent = if slider_mode == "probability" {
        number(&["store", "randomPercent"])
            .or_else(|| number(&["store", "contextSliderPos"]))
            .unwrap_or(100.0)
            .clamp(0.0, 100.0)
    } else if let Some(position) = number(&["store", "contextSliderPos"]) {
        legacy_slider_to_probability(position)
    } else {
        let legacy_probability = number(&["store", "randomPercent"])
            .unwrap_or(100.0)
            .clamp(0.0, 100.0);
        match number(&["store", "contextTier"]).map(|tier| tier.round() as i32) {
            Some(tier) if tier >= 4 => 100.0,
            Some(tier) if tier <= 2 => 0.0,
            _ => legacy_probability,
        }
    };
    let unified_tier = boolean(&["store", "unifiedTier"]).unwrap_or(true);
    let group_random_percent = if unified_tier {
        std::collections::BTreeMap::new()
    } else {
        object
            .get("store")
            .and_then(|store| store.get("groupSliderPos"))
            .and_then(serde_json::Value::as_object)
            .map(|groups| {
                groups
                    .iter()
                    .filter_map(|(group_id, raw)| {
                        let value = raw.as_f64()?;
                        let probability = if slider_mode == "probability" {
                            value.clamp(0.0, 100.0)
                        } else {
                            legacy_slider_to_probability(value)
                        };
                        Some((group_id.clone(), probability))
                    })
                    .collect::<std::collections::BTreeMap<_, _>>()
            })
            .unwrap_or_default()
    };
    let provider_embedded_key = object
        .get("providers")
        .and_then(serde_json::Value::as_array)
        .and_then(|providers| {
            providers.iter().find(|provider| {
                provider.get("id").and_then(serde_json::Value::as_str)
                    == Some(selected_provider.as_str())
            })
        })
        .and_then(|provider| provider.get("apiKey"))
        .and_then(serde_json::Value::as_str)
        .filter(|key| !key.trim().is_empty() && *key != "******")
        .map(str::to_owned);
    let model_api_key = string(&["api", "apiKey"])
        .filter(|key| !key.trim().is_empty() && key != "******")
        .or(selected_provider_key.filter(|key| !key.trim().is_empty() && key != "******"))
        .or(provider_embedded_key);
    let fallback_value = object.get("api").and_then(|api| api.get("fallback"));
    let fallback_model = fallback_value
        .and_then(|fallback| fallback.get("model"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let fallback_enabled = !fallback_model.trim().is_empty()
        && fallback_value
            .and_then(|fallback| fallback.get("enabled"))
            .and_then(serde_json::Value::as_bool)
            != Some(false);
    let fallback_base_url = fallback_value
        .and_then(|fallback| fallback.get("baseUrl"))
        .and_then(serde_json::Value::as_str)
        .filter(|url| !url.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| string(&["api", "baseUrl"]))
        .unwrap_or_default();
    let fallback_api_key = fallback_value
        .and_then(|fallback| fallback.get("apiKey"))
        .and_then(serde_json::Value::as_str)
        .filter(|key| !key.trim().is_empty() && *key != "******")
        .map(str::to_owned)
        .or_else(|| model_api_key.clone());
    let model_timeout_ms =
        node_model_timeout_ms(object.get("api").and_then(|api| api.get("timeoutMs")))
            .unwrap_or(60_000);
    let fallback_timeout_ms =
        node_model_timeout_ms(fallback_value.and_then(|fallback| fallback.get("timeoutMs")))
            .unwrap_or(model_timeout_ms);
    let vision_value = object.get("api").and_then(|api| api.get("visionModel"));
    let vision_provider = vision_value
        .and_then(|setting| setting.get("provider"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let vision_provider_entry = object
        .get("providers")
        .and_then(serde_json::Value::as_array)
        .and_then(|providers| {
            providers.iter().find(|provider| {
                provider.get("id").and_then(serde_json::Value::as_str) == Some(vision_provider)
            })
        });
    let model_vision = vision_value.and_then(|setting| {
        let model = setting
            .get("model")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim();
        let base_url = setting
            .get("baseUrl")
            .and_then(serde_json::Value::as_str)
            .filter(|url| !url.trim().is_empty())
            .or_else(|| {
                vision_provider_entry
                    .and_then(|provider| provider.get("baseURL"))
                    .and_then(serde_json::Value::as_str)
                    .filter(|url| !url.trim().is_empty())
            })?;
        (!model.is_empty()).then(|| VisionModelSettings {
            enabled: setting.get("enabled").and_then(serde_json::Value::as_bool) != Some(false)
                && object
                    .get("api")
                    .and_then(|api| api.get("vision"))
                    .and_then(serde_json::Value::as_bool)
                    != Some(false),
            base_url: base_url.to_owned(),
            model: model.to_owned(),
            timeout_ms: setting
                .get("timeoutMs")
                .and_then(serde_json::Value::as_u64)
                .filter(|timeout| (1_000..=300_000).contains(timeout))
                .unwrap_or(90_000),
        })
    });
    let model_vision_api_key = vision_value.and_then(|setting| {
        setting
            .get("apiKey")
            .and_then(serde_json::Value::as_str)
            .filter(|key| !key.trim().is_empty() && *key != "******")
            .map(str::to_owned)
            .or_else(|| {
                object
                    .get("providerKeys")
                    .and_then(|keys| keys.get(vision_provider))
                    .and_then(serde_json::Value::as_str)
                    .filter(|key| !key.trim().is_empty() && *key != "******")
                    .map(str::to_owned)
            })
            .or_else(|| {
                vision_provider_entry
                    .and_then(|provider| provider.get("apiKey"))
                    .and_then(serde_json::Value::as_str)
                    .filter(|key| !key.trim().is_empty() && *key != "******")
                    .map(str::to_owned)
            })
    });
    let asr_settings = object.get("asr").map(|asr| {
        let provider = asr
            .get("provider")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("openai")
            .trim()
            .to_ascii_lowercase();
        AsrSettings {
            enabled: asr
                .get("enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
            provider,
            base_url: asr
                .get("baseUrl")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            model: asr
                .get("model")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            language: asr
                .get("language")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            max_per_hour: asr
                .get("maxPerHour")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(12)
                .clamp(1, 200) as u32,
            timeout_ms: asr
                .get("timeoutMs")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(300_000)
                .clamp(1_000, 600_000),
        }
    });
    let asr_api_key = object.get("asr").and_then(|asr| {
        let provider = asr
            .get("provider")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("openai")
            .trim()
            .to_ascii_lowercase();
        let stored_for = asr
            .get("apiKeyProvider")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let base_url = asr
            .get("baseUrl")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let bound_host = asr
            .get("apiKeyHost")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let current_host = reqwest::Url::parse(base_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
            .unwrap_or_default();
        if !stored_for.is_empty() && stored_for != provider {
            return None;
        }
        if provider == "openai" && !bound_host.is_empty() && bound_host != current_host {
            return None;
        }
        asr.get("apiKey")
            .and_then(serde_json::Value::as_str)
            .filter(|key| !key.trim().is_empty() && *key != "******")
            .map(str::to_owned)
    });
    let role = string(&["persona", "roleText"]).unwrap_or_default();
    let custom_rules = string(&["persona", "customRules"]).unwrap_or_default();
    let bounded_persona_field = |key: &str, maximum: usize| {
        string(&["persona", key]).map(|value| value.chars().take(maximum).collect::<String>())
    };
    let daily_moments_persona =
        object
            .get("persona")
            .map(|_| crate::daily_moments::DailyMomentPersona {
                bot_name: bounded_persona_field("botName", 100).unwrap_or_default(),
                self_nickname: bounded_persona_field("selfNickname", 100).unwrap_or_default(),
                self_appearance: bounded_persona_field("selfAppearance", 4_000).unwrap_or_default(),
                role_text: bounded_persona_field("roleText", 20_000).unwrap_or_default(),
                custom_rules: bounded_persona_field("customRules", 8_000).unwrap_or_default(),
                participation: bounded_persona_field("participation", 4_000).unwrap_or_default(),
            });
    let prompt = match (role.is_empty(), custom_rules.is_empty()) {
        (true, true) => None,
        (false, true) => Some(role),
        (true, false) => Some(custom_rules),
        (false, false) => Some(format!("{role}\n\n附加规则：\n{custom_rules}")),
    };
    let time_control_json = object
        .get("timeControl")
        .and_then(|value| serde_json::to_string(value).ok());
    let daily_moments = object
        .get("dailyMoments")
        .map(|value| {
            use crate::daily_moments::DailyMomentsSettings;

            let Some(settings) = value.as_object() else {
                return None;
            };
            let number = |key: &str| settings.get(key).and_then(serde_json::Value::as_f64);
            let boolean = |key: &str| settings.get(key).and_then(serde_json::Value::as_bool);
            let schedule_windows = match settings.get("scheduleWindows") {
                None | Some(serde_json::Value::Null) => None,
                Some(value @ serde_json::Value::Array(_)) => {
                    Some(serde_json::from_value(value.clone()).ok()?)
                }
                _ => return None,
            };
            let target_uins = settings
                .get("targetUins")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|value| match value {
                    serde_json::Value::String(uin) => Some(uin.clone()),
                    serde_json::Value::Number(uin) => uin.as_u64().map(|value| value.to_string()),
                    _ => None,
                })
                .fold(Vec::new(), |mut unique, uin| {
                    if unique.len() < 200 && !unique.contains(&uin) {
                        unique.push(uin);
                    }
                    unique
                });
            let fallback = DailyMomentsSettings::default();
            let bounded = |key: &str, default: f64, minimum: f64, maximum: f64| {
                number(key)
                    .filter(|value| value.is_finite())
                    .map(|value| value.round().clamp(minimum, maximum))
                    .unwrap_or(default)
            };
            let settings = DailyMomentsSettings {
                enabled: boolean("enabled").unwrap_or(false),
                hour: bounded("hour", f64::from(fallback.hour), 0.0, 23.0) as u8,
                minute: bounded("minute", f64::from(fallback.minute), 0.0, 59.0) as u8,
                schedule_windows,
                interval_days: bounded("intervalDays", 1.0, 1.0, 30.0) as u8,
                startup_catchup: boolean("startupCatchup").unwrap_or(true),
                min_messages_per_group: bounded(
                    "minMessagesPerGroup",
                    f64::from(fallback.min_messages_per_group),
                    0.0,
                    100.0,
                ) as u8,
                max_groups: bounded("maxGroups", 12.0, 1.0, 50.0) as u8,
                max_messages_per_group: bounded("maxMessagesPerGroup", 80.0, 5.0, 300.0) as u16,
                max_prompt_chars: bounded("maxPromptChars", 120_000.0, 20_000.0, 500_000.0) as u32,
                allow_images: boolean("allowImages").unwrap_or(true),
                max_images: bounded("maxImages", f64::from(fallback.max_images), 0.0, 4.0) as u8,
                visibility: number("visibility")
                    .filter(|value| [1.0, 4.0, 16.0, 64.0, 128.0].contains(value))
                    .unwrap_or(4.0) as u16,
                target_uins,
                max_research_calls: bounded(
                    "maxResearchCalls",
                    f64::from(fallback.max_research_calls),
                    0.0,
                    10.0,
                ) as u8,
                max_rounds: bounded("maxRounds", 8.0, 2.0, 16.0) as u8,
            };
            settings.validate().ok()?;
            Some(settings)
        })
        .flatten();
    let config = ImportedNodeRuntimeConfig {
        schema_version: 1,
        unmigrated_enabled_features: None,
        enabled_feature_data_missing: None,
        // `applyStableFeaturePolicy` forces identity on and graduated, while
        // incoming friend handling remains an independently persisted switch.
        identity_pilot_enabled: Some(true),
        memory_handoff_enabled: Some(boolean(&["memory", "handoffEnabled"]).unwrap_or(true)),
        multimodal_context_enabled: Some(
            boolean(&["multimodalContextPilot", "enabled"]).unwrap_or(false),
        ),
        multimodal_context_graduated: Some(
            boolean(&["multimodalContextPilot", "graduated"]).unwrap_or(false),
        ),
        memory_consolidate_enabled: Some(
            boolean(&["memory", "consolidateEnabled"]).unwrap_or(true),
        ),
        memory_consolidate_min_impressions: number(&["memory", "consolidateMinImpressions"])
            .map(|value| value.round().clamp(1.0, 1000.0) as usize),
        memory_consolidate_max_per_member: number(&["memory", "maxImpressionsPerMember"])
            .map(|value| value.round().clamp(2.0, 1000.0) as usize),
        memory_consolidate_discover_min_messages: number(&["memory", "discoverMinMessages"])
            .map(|value| value.round().clamp(1.0, 500.0) as usize),
        memory_consolidate_discover_max_members: number(&["memory", "discoverMaxMembers"])
            .map(|value| value.round().clamp(1.0, 20.0) as usize),
        memory_consolidate_min_interval_ms: Some(
            number(&["memory", "consolidateMinIntervalMs"])
                .map(|value| value.round().clamp(1_800_000.0, 31_536_000_000.0) as i64)
                .unwrap_or(21_600_000),
        ),
        memory_handoff_ttl_minutes: Some(
            number(&["memory", "handoffTtlMinutes"])
                .map(|value| value.round().clamp(5.0, 10_080.0) as u32)
                .unwrap_or(1440),
        ),
        memory_handoff_max_chars: Some(memory_handoff_max_chars),
        identity_pilot_graduated: Some(true),
        incoming_friend_request_enabled: Some(
            boolean(&["identityPilot", "incomingFriendRequest", "enabled"]).unwrap_or(false),
        ),
        incoming_friend_auto_whitelist: boolean(&[
            "identityPilot",
            "incomingFriendRequest",
            "autoWhitelist",
        ]),
        incoming_friend_max_pending: number(&[
            "identityPilot",
            "incomingFriendRequest",
            "maxPending",
        ])
        .map(|value| value as u32),
        group_invite_enabled: Some(boolean(&["groupInvite", "enabled"]).unwrap_or(false)),
        group_invite_auto_whitelist: Some(
            boolean(&["groupInvite", "autoWhitelist"]).unwrap_or(true),
        ),
        group_invite_friends_only: Some(boolean(&["groupInvite", "friendsOnly"]).unwrap_or(true)),
        group_invite_notify_owner: Some(boolean(&["groupInvite", "notifyOwner"]).unwrap_or(true)),
        group_invite_owner_uin: string(&["admin", "ownerUin"]),
        web_search_enabled: Some(boolean(&["webSearch", "enabled"]).unwrap_or(true)),
        web_search_provider: string(&["webSearch", "provider"]),
        web_search_url: string(&["webSearch", "searchUrl"]),
        web_search_max_results: number(&["webSearch", "maxResults"]).map(|value| value as usize),
        identity_owner_uin: string(&["identityPilot", "friendProposal", "ownerUin"]),
        admin_owner_uin: string(&["admin", "ownerUin"]),
        incident_pilot_enabled: Some(boolean(&["incidentPilot", "enabled"]).unwrap_or(false)),
        incident_pilot_graduated: Some(boolean(&["incidentPilot", "graduated"]).unwrap_or(false)),
        incident_owner_uin: string(&["incidentPilot", "ownerUin"]),
        incident_notify_warnings: Some(
            boolean(&["incidentPilot", "notifyWarnings"]).unwrap_or(true),
        ),
        incident_duplicate_window_minutes: number(&["incidentPilot", "duplicateWindowMinutes"])
            .map(|value| value.round().clamp(1.0, 1440.0) as u32),
        incident_unknown_writes_block_chat: Some(
            boolean(&["incidentPilot", "unknownWritesBlockChat"]).unwrap_or(false),
        ),
        incident_retention_days: number(&["incidentPilot", "retentionDays"])
            .map(|value| value.round().clamp(1.0, 3650.0) as u32),
        relationship_pilot: Some(RelationshipPilotSettings {
            enabled: boolean(&["relationshipPilot", "enabled"]).unwrap_or(false),
            graduated: boolean(&["relationshipPilot", "graduated"]).unwrap_or(false),
            model: string(&["relationshipPilot", "model"])
                .or_else(|| string(&["api", "model"]))
                .unwrap_or_default(),
            min_new_messages: number(&["relationshipPilot", "minNewMessages"])
                .map(|v| v.round().clamp(4.0, 50.0) as usize)
                .unwrap_or(8),
            min_batch_messages: number(&["relationshipPilot", "minBatchMessages"])
                .map(|v| v.round().clamp(2.0, 20.0) as usize)
                .unwrap_or(3),
            min_batch_age_minutes: number(&["relationshipPilot", "minBatchAgeMinutes"])
                .map(|v| v.round().clamp(30.0, 1440.0) as u64)
                .unwrap_or(360),
            max_evidence_messages: number(&["relationshipPilot", "maxEvidenceMessages"])
                .map(|v| v.round().clamp(8.0, 64.0) as u32)
                .unwrap_or(32),
            max_people_per_turn: number(&["relationshipPilot", "maxPeoplePerTurn"])
                .map(|v| v.round().clamp(1.0, 4.0) as usize)
                .unwrap_or(2),
            max_evaluations_per_day: number(&["relationshipPilot", "maxEvaluationsPerDay"])
                .map(|v| v.round().clamp(1.0, 200.0) as u32)
                .unwrap_or(20),
            familiarity_refresh_minutes: number(&[
                "relationshipPilot",
                "familiarityRefreshMinutes",
            ])
            .map(|v| v.round().clamp(1.0, 1440.0) as u64)
            .unwrap_or(5),
            friction_half_life_hours: number(&["relationshipPilot", "frictionHalfLifeHours"])
                .map(|v| v.clamp(6.0, 720.0))
                .unwrap_or(48.0),
            max_memory_context: number(&["relationshipPilot", "maxMemoryContext"])
                .map(|v| v.round().clamp(0.0, 12.0) as usize)
                .unwrap_or(6),
        }),
        qzone_interactions: object
            .get("qzoneInteractions")
            .and_then(map_qzone_interactions_settings)
            .map(|mut settings| {
                settings.decision_input_budget_tokens = node_qzone_input_budget(value);
                settings.thinking_request_patch = node_qzone_thinking_patch(value);
                settings
            }),
        proactive: Some(map_node_proactive_settings(value)),
        max_concurrent_runs: Some(
            number(&["maxConcurrentRuns"])
                .map(|value| value.ceil().max(1.0) as usize)
                .unwrap_or(2),
        ),
        daily_moments,
        daily_moments_persona,
        sticker_enabled: Some(boolean(&["sticker", "enabled"]).unwrap_or(true)),
        sticker_collect_enabled: Some(boolean(&["sticker", "collectEnabled"]).unwrap_or(true)),
        sticker_auto_collect_enabled: Some(boolean(&["sticker", "autoCollect"]).unwrap_or(true)),
        sticker_save_to_qq_favorites: Some(
            boolean(&["sticker", "saveToQqFavorites"]).unwrap_or(true),
        ),
        sticker_max_collect_per_hour: Some(
            number(&["sticker", "maxCollectPerHour"])
                .map(|value| value.round().clamp(1.0, 100.0) as u32)
                .unwrap_or(10),
        ),
        sticker_prompt_max_stickers: Some(sticker_prompt_max_stickers),
        sticker_encourage: Some(
            number(&["sticker", "encourage"])
                .map(|value| value.round().clamp(0.0, 3.0) as u8)
                .unwrap_or(1),
        ),
        send_max_per_minute: Some(
            number(&["send", "maxPerMinute"])
                .map(|value| value.round().clamp(1.0, 10_000.0) as usize)
                .unwrap_or(80),
        ),
        send_max_per_hour: Some(
            number(&["send", "maxPerHour"])
                .map(|value| value.round().clamp(1.0, 100_000.0) as usize)
                .unwrap_or(500),
        ),
        send_min_gap_ms: Some(
            number(&["send", "minGapMs"])
                .map(|value| {
                    if value == 0.0 {
                        1_000
                    } else {
                        value.round().clamp(200.0, 15_000.0) as u64
                    }
                })
                .unwrap_or(1_000),
        ),
        send_max_gap_ms: Some(
            number(&["send", "maxGapMs"])
                .map(|value| {
                    if value == 0.0 {
                        3_000
                    } else {
                        value.round().clamp(200.0, 15_000.0) as u64
                    }
                })
                .unwrap_or(3_000),
        ),
        send_by_length_ms: Some(
            number(&["send", "byLengthMs"])
                .map(|value| {
                    if value == 0.0 {
                        20
                    } else {
                        value.round().clamp(1.0, 1_000.0) as u64
                    }
                })
                .unwrap_or(20),
        ),
        send_hard_split_at: Some(
            number(&["send", "hardSplitAt"])
                .map(|value| value.round().clamp(0.0, 100_000.0) as usize)
                .unwrap_or(4_000),
        ),
        blocklist: Some(blocklist),
        unified_tier: Some(unified_tier),
        group_random_percent: Some(group_random_percent),
        onebot_ws_url: string(&["onebot", "wsUrl"]),
        onebot_http_url: string(&["onebot", "httpUrl"]),
        onebot_token: string(&["onebot", "accessToken"]),
        model_base_url: string(&["api", "baseUrl"]),
        model_id: string(&["api", "model"]),
        model_api_key,
        model_timeout_ms: Some(model_timeout_ms),
        model_fallback: Some(FallbackModelSettings {
            enabled: fallback_enabled,
            base_url: fallback_base_url,
            model: fallback_model,
            timeout_ms: fallback_timeout_ms,
        }),
        model_fallback_api_key: fallback_api_key,
        model_vision,
        model_vision_api_key,
        asr: asr_settings,
        asr_api_key,
        system_prompt: prompt,
        allow_private: id_list(&["allow", "private"]),
        allow_groups: id_list(&["allow", "groups"]),
        deny_private: id_list(&["deny", "private"]),
        deny_groups: id_list(&["deny", "groups"]),
        random_response_percent: Some(random_response_percent),
        trigger_keywords: keywords,
        at_context_count: number(&["store", "atCount"]).map(|v| v as usize),
        keyword_context_count: number(&["store", "keywordCount"]).map(|v| v as usize),
        random_context_count: number(&["store", "randomCount"]).map(|v| v as usize),
        all_context_count: number(&["store", "allCount"]).map(|v| v as usize),
        wake_delay_min_ms: number(&["wakeDelayMinMs"]).map(|v| v as u64),
        wake_delay_max_ms: number(&["wakeDelayMaxMs"]).map(|v| v as u64),
        max_batch_wait_ms: number(&["maxBatchWaitMs"]).map(|v| v as u64),
        batch_limit: number(&["store", "batchLimit"]).map(|v| v as u32),
        batch_max_chars: number(&["store", "batchMaxChars"]).map(|v| v as usize),
        time_control_json,
        model_temperature: number(&["api", "temperature"]).map(|v| v as f32),
        model_max_tokens: None,
        model_max_rounds: number(&["api", "maxRounds"]).map(|v| v.round().clamp(1.0, 64.0) as u32),
    };
    Some(config)
}

fn node_qzone_input_budget(config: &serde_json::Value) -> u32 {
    let normalized_number = |path: &[&str], fallback: f64, minimum: f64, maximum: f64| {
        path.iter()
            .try_fold(config, |current, key| current.get(*key))
            .and_then(serde_json::Value::as_f64)
            .filter(|number| number.is_finite() && *number != 0.0)
            .unwrap_or(fallback)
            .round()
            .clamp(minimum, maximum) as u32
    };
    let mode = config
        .pointer("/tokenSaver/mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("off")
        .trim()
        .to_ascii_lowercase();
    let run_cap = match mode.as_str() {
        "balanced" => 80_000,
        "aggressive" => 50_000,
        _ => 1_000_000,
    };
    let max_run = normalized_number(&["api", "maxRunTokens"], 160_000.0, 20_000.0, 1_000_000.0)
        .min(run_cap)
        .max(20_000);
    let context = normalized_number(
        &["api", "contextWindowTokens"],
        1_000_000.0,
        16_000.0,
        1_000_000.0,
    );
    context.min(max_run).saturating_sub(8_192).max(4_000)
}

fn map_node_proactive_settings(
    value: &serde_json::Value,
) -> crate::application::proactive::ProactiveSettings {
    use crate::application::proactive::{map_node_active_hours, ProactiveSettings};
    let defaults = ProactiveSettings::default();
    let object = value
        .get("proactive")
        .and_then(serde_json::Value::as_object);
    let number = |key: &str| {
        object
            .and_then(|object| object.get(key))
            .and_then(serde_json::Value::as_f64)
    };
    let boolean = |key: &str| {
        object
            .and_then(|object| object.get(key))
            .and_then(serde_json::Value::as_bool)
    };
    let node_number = |key: &str, default: u64| {
        number(key)
            .filter(|value| *value != 0.0)
            .map(|value| value.round().clamp(0.0, u64::MAX as f64) as u64)
            .unwrap_or(default)
    };
    ProactiveSettings {
        enabled: boolean("enabled").unwrap_or(false),
        // Node has no graduation state; importing never silently graduates a Rust feature.
        graduated: false,
        check_interval_min_ms: node_number("checkIntervalMinMs", defaults.check_interval_min_ms)
            .max(60_000),
        check_interval_max_ms: node_number("checkIntervalMaxMs", defaults.check_interval_max_ms)
            .max(120_000),
        idle_threshold_ms: node_number("idleThresholdMs", defaults.idle_threshold_ms).max(300_000),
        probability: number("probability")
            .filter(|value| *value != 0.0)
            .unwrap_or(defaults.probability),
        follow_up_enabled: boolean("followUpEnabled").unwrap_or(true),
        self_wake_enabled: boolean("selfWakeEnabled").unwrap_or(true),
        active_hours: map_node_active_hours(object.and_then(|object| object.get("activeHours"))),
    }
}

fn map_qzone_interactions_settings(
    value: &serde_json::Value,
) -> Option<crate::application::qzone_interactions::QzoneInteractionsSettings> {
    use crate::application::qzone_interactions::{QzoneActiveHours, QzoneInteractionsSettings};
    let mut settings = QzoneInteractionsSettings::default();
    let object = value.as_object()?;
    let number = |key: &str| object.get(key).and_then(serde_json::Value::as_f64);
    let boolean = |key: &str| object.get(key).and_then(serde_json::Value::as_bool);
    let bounded = |key: &str, fallback: u32, min: u32, max: u32| {
        number(key)
            .map(|value| value.round().clamp(f64::from(min), f64::from(max)) as u32)
            .unwrap_or(fallback)
    };
    settings.enabled = boolean("enabled").unwrap_or(false);
    settings.startup_catchup = boolean("startupCatchup").unwrap_or(false);
    settings.active_hours = object
        .get("activeHours")
        .and_then(serde_json::Value::as_object)
        .and_then(|hours| {
            Some(QzoneActiveHours {
                start: hours.get("start")?.as_str()?.to_owned(),
                end: hours.get("end")?.as_str()?.to_owned(),
            })
        });
    settings.feed_interval_minutes = bounded("feedIntervalMinutes", 60, 5, 1440);
    settings.reply_interval_minutes = bounded("replyIntervalMinutes", 5, 1, 1440);
    settings.feed_fetch_count = bounded("feedFetchCount", 30, 1, 50);
    settings.own_post_count = bounded("ownPostCount", 10, 1, 30);
    settings.max_age_hours = bounded("maxAgeHours", 72, 1, 720);
    settings.max_batch_items = bounded("maxBatchItems", 20, 1, 50);
    settings.max_likes_per_run = bounded("maxLikesPerRun", 3, 0, 20);
    settings.max_comments_per_run = bounded("maxCommentsPerRun", 2, 0, 10);
    settings.max_replies_per_run = bounded("maxRepliesPerRun", 5, 0, 20);
    settings.comment_max_chars = bounded("commentMaxChars", 60, 5, 200);
    settings.reply_max_chars = bounded("replyMaxChars", 60, 5, 200);
    settings.allow_likes = boolean("allowLikes") != Some(false);
    settings.allow_comments = boolean("allowComments") != Some(false);
    settings.allow_replies = boolean("allowReplies") != Some(false);
    settings.action_delay_min_ms = bounded("actionDelayMinMs", 700, 0, 10_000);
    settings.action_delay_max_ms = bounded("actionDelayMaxMs", 1800, 0, 15_000);
    settings.max_decision_rounds = bounded("maxDecisionRounds", 3, 1, 5);
    // Node treats absent/invalid active-hour windows as unrestricted. Keep that
    // behavior while still rejecting malformed windows at the Rust API edge.
    if settings.active_hours.as_ref().is_some_and(|hours| {
        let probe = QzoneInteractionsSettings {
            active_hours: Some(hours.clone()),
            ..QzoneInteractionsSettings::default()
        };
        probe.validate().is_err()
    }) {
        settings.active_hours = None;
    }
    settings.validate().ok()?;
    Some(settings)
}

fn legacy_slider_to_probability(position: f64) -> f64 {
    let position = position.clamp(0.0, 100.0);
    if position <= 20.0 {
        0.0
    } else if position >= 90.0 {
        100.0
    } else {
        ((position - 20.0) / 70.0 * 1000.0).round() / 10.0
    }
}

fn read_all_legacy_records(
    store: &SqliteStore,
    domain: &str,
    table: &str,
) -> Result<Vec<serde_json::Value>, ImportError> {
    let mut offset = 0_u32;
    let mut output = Vec::new();
    loop {
        let (page, total) = store
            .legacy_domain_records(domain, table, 1000, offset)
            .map_err(|_| ImportError::ImportFailed)?;
        if page.is_empty() {
            if i64::from(offset) < total {
                return Err(ImportError::ImportFailed);
            }
            break;
        }
        offset = offset.saturating_add(page.len() as u32);
        output.extend(page);
        if i64::from(offset) >= total || offset >= 1_000_000 {
            if i64::from(offset) < total {
                return Err(ImportError::ImportFailed);
            }
            break;
        }
    }
    Ok(output)
}

fn current_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn verify_target(
    path: &Path,
    summary: &LegacyImportSummary,
    sessions: &LegacySessionImportSummary,
    pilot_databases: &[LegacyDomainImportSummary],
) -> rusqlite::Result<(bool, u64, u64)> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    let target_messages: i64 =
        connection.query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))?;
    let target_outbox: i64 =
        connection.query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))?;
    let target_runs: i64 =
        connection.query_row("SELECT COUNT(*) FROM legacy_runs", [], |row| row.get(0))?;
    let target_recall_notifications: i64 = connection.query_row(
        "SELECT COUNT(*) FROM recall_notifications WHERE status='sent'",
        [],
        |row| row.get(0),
    )?;
    let target_sessions: i64 =
        connection.query_row("SELECT COUNT(*) FROM legacy_sessions", [], |row| row.get(0))?;
    for database in pilot_databases {
        if database.verified_rows != database.total_rows
            || database.tables.iter().any(|table| {
                table.source_rows != table.imported_rows || table.source_rows != table.verified_rows
            })
        {
            return Ok((false, 0, 0));
        }
        let target_rows: i64 = connection.query_row(
            "SELECT COUNT(*) FROM legacy_domain_records WHERE domain=?1",
            [&database.domain],
            |row| row.get(0),
        )?;
        if target_rows != database.total_rows as i64 {
            return Ok((false, 0, 0));
        }
    }
    if target_messages != summary.messages as i64
        || target_outbox != summary.outbox as i64
        || target_runs != summary.runs as i64
        || target_recall_notifications != summary.recall_notifications_imported as i64
        || target_sessions != sessions.imported as i64
        || summary.verified_message_rows != summary.messages
        || summary.verified_outbox_rows != summary.outbox
        || summary.verified_run_rows != summary.runs
        || summary.verified_recall_notification_rows != summary.recall_notifications_imported
    {
        return Ok((false, 0, 0));
    }
    Ok((
        integrity == "ok",
        summary.verified_message_samples,
        summary.verified_outbox_samples,
    ))
}

fn restrict_tree_permissions(path: &Path) -> std::io::Result<()> {
    set_directory_private(path)?;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(std::io::Error::other("unexpected symlink in staged import"));
        }
        if kind.is_dir() {
            restrict_tree_permissions(&entry.path())?;
        } else if kind.is_file() {
            restrict_file_permissions(&entry.path())?;
        }
    }
    Ok(())
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

fn restrict_file_permissions(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn private_new_file(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::{
        effective_node_enabled_features, fingerprint_normalized_data_tree,
        import_memory_consolidation_backup_archive, import_memory_consolidation_cooldowns,
        import_proactive_state, resolve_name_only_backup_user_id,
    };
    use std::{fs, path::Path};

    fn write_json(path: &Path, value: serde_json::Value) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    }

    #[test]
    fn normalized_data_fingerprint_binds_runtime_files_but_excludes_legacy_and_report() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(root.join("rinbot.sqlite"), b"runtime db v1").unwrap();
        fs::create_dir(root.join("legacy")).unwrap();
        fs::write(root.join("legacy/source.json"), b"source archive v1").unwrap();
        fs::write(root.join("migration-report.json"), b"report v1").unwrap();
        let initial = fingerprint_normalized_data_tree(root).unwrap();

        fs::write(root.join("legacy/source.json"), b"source archive changed").unwrap();
        fs::write(root.join("migration-report.json"), b"report changed").unwrap();
        assert_eq!(fingerprint_normalized_data_tree(root).unwrap(), initial);

        fs::write(root.join("rinbot.sqlite"), b"runtime db v2").unwrap();
        assert_ne!(fingerprint_normalized_data_tree(root).unwrap(), initial);
    }

    #[test]
    fn node_consolidation_backups_are_imported_for_rust_restore_history() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("legacy/memory/backups/consolidation/123");
        write_json(
            &archive.join("1700000000000-snapshot.json"),
            serde_json::json!({
                "version": 1,
                "reason": "consolidation",
                "sourceChatKey": "group:7",
                "backedUpAt": 1700000000000_i64,
                "person": {
                    "userId": "123",
                    "impressions": [
                        {"content":"喜欢露营", "sourceChatKeys":["group:7"]},
                        {"content":"偏好短消息", "sourceChatKeys":["private:123"]}
                    ]
                }
            }),
        );

        let summary = import_memory_consolidation_backup_archive(
            &temp.path().join("legacy/memory/backups/consolidation"),
            &temp.path().join("identity.sqlite"),
            &temp.path().join("identity-pilot.sqlite"),
            1700000001000,
        )
        .unwrap();
        assert!(summary.archive_present);
        assert_eq!(summary.source_files, 1);
        assert_eq!(summary.verified_files, 1);
        assert_eq!(summary.person_snapshots, 1);
        assert_eq!(summary.impressions, 2);
        assert!(summary.preserved_in_legacy_archive);
        assert_eq!(summary.imported_restorable_files, 1);
        assert!(summary.available_to_rust_restore_ui);
        let identity =
            crate::identity::IdentityStore::open(temp.path().join("identity.sqlite")).unwrap();
        let history = identity
            .node_memory_consolidation_backup_history("123", 20)
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].backup_key, "123/1700000000000-snapshot.json");
        assert_eq!(history[0].impressions.len(), 2);
        assert!(history[0]
            .impressions
            .iter()
            .any(|item| item.source_chat_keys == ["group:7"]));
    }

    #[test]
    fn name_only_node_backup_maps_only_to_a_unique_identity_in_its_source_chat() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("memory/backups/consolidation/_n_小明");
        write_json(
            &archive.join("1700000000000-snapshot.json"),
            serde_json::json!({
                "version":1,
                "reason":"manual-delete",
                "sourceChatKey":"group:7",
                "backedUpAt":1700000000000_i64,
                "person":{
                    "userId":"",
                    "name":"小明",
                    "sourceChatKeys":["group:7"],
                    "impressions":[{"content":"喜欢露营","sourceChatKeys":["group:7"]}]
                }
            }),
        );
        let identity_source =
            rusqlite::Connection::open(temp.path().join("identity-pilot.sqlite")).unwrap();
        identity_source
            .execute_batch(
                "CREATE TABLE people(uin TEXT PRIMARY KEY,primary_name TEXT NOT NULL);
                 CREATE TABLE identity_sources(uin TEXT NOT NULL,chat_key TEXT NOT NULL);
                 CREATE TABLE identity_aliases(uin TEXT NOT NULL,chat_key TEXT NOT NULL,alias TEXT NOT NULL);
                 INSERT INTO people VALUES('123','小明'),('456','小明');
                 INSERT INTO identity_sources VALUES('123','group:7'),('456','group:8');",
            )
            .unwrap();
        drop(identity_source);

        let summary = import_memory_consolidation_backup_archive(
            &temp.path().join("memory/backups/consolidation"),
            &temp.path().join("identity.sqlite"),
            &temp.path().join("identity-pilot.sqlite"),
            1700000001000,
        )
        .unwrap();
        assert_eq!(summary.resolved_name_only_files, 1);
        assert_eq!(summary.restorable_files, 1);
        assert_eq!(summary.imported_restorable_files, 1);
        assert!(summary.available_to_rust_restore_ui);
        let identity =
            crate::identity::IdentityStore::open(temp.path().join("identity.sqlite")).unwrap();
        let history = identity
            .node_memory_consolidation_backup_history("123", 20)
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].backup_key, "123/1700000000000-snapshot.json");
        assert_eq!(history[0].impressions.len(), 1);
    }

    #[test]
    fn name_only_backup_with_ambiguous_identity_stays_archived() {
        let temp = tempfile::tempdir().unwrap();
        let identity_source =
            rusqlite::Connection::open(temp.path().join("identity-pilot.sqlite")).unwrap();
        identity_source
            .execute_batch(
                "CREATE TABLE people(uin TEXT PRIMARY KEY,primary_name TEXT NOT NULL);
                 CREATE TABLE identity_sources(uin TEXT NOT NULL,chat_key TEXT NOT NULL);
                 CREATE TABLE identity_aliases(uin TEXT NOT NULL,chat_key TEXT NOT NULL,alias TEXT NOT NULL);
                 INSERT INTO people VALUES('123','重名用户'),('456','重名用户');
                 INSERT INTO identity_sources VALUES('123','group:7'),('456','group:8');",
            )
            .unwrap();
        drop(identity_source);
        let candidates = resolve_name_only_backup_user_id(
            &temp.path().join("identity-pilot.sqlite"),
            "重名用户",
            &[],
        )
        .unwrap();
        assert_eq!(candidates, None);
    }

    #[test]
    fn disabled_proactive_import_preserves_legacy_state_without_creating_feature_store() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("proactive-state.json");
        let destination = temp.path().join("proactive.sqlite");
        fs::write(&source, r#"{"lastAttemptAt":1700000007000}"#).unwrap();

        let summary = import_proactive_state(&source, &destination, false).unwrap();

        assert!(summary.source_present);
        assert!(!summary.imported);
        assert!(!summary.verified);
        assert!(!destination.exists());
    }

    #[test]
    fn malformed_node_consolidation_backup_aborts_import_audit() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("memory/backups/consolidation/123");
        write_json(
            &archive.join("bad.json"),
            serde_json::json!({"version": 1, "reason": "consolidation"}),
        );
        assert!(import_memory_consolidation_backup_archive(
            &temp.path().join("memory/backups/consolidation"),
            &temp.path().join("identity.sqlite"),
            &temp.path().join("identity-pilot.sqlite"),
            1,
        )
        .is_err());
    }

    #[test]
    fn unmatched_name_only_node_backups_stay_archived_and_are_reported_unrestorable() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("memory/backups/consolidation/_n_unknown");
        write_json(
            &archive.join("1700000000000-name-only.json"),
            serde_json::json!({
                "version":1,
                "reason":"manual-delete",
                "sourceChatKey":"",
                "backedUpAt":1700000000000_i64,
                "person":{"userId":"","name":"重名用户","impressions":[{"content":"未能确认身份的历史印象"}]}
            }),
        );
        let summary = import_memory_consolidation_backup_archive(
            &temp.path().join("memory/backups/consolidation"),
            &temp.path().join("identity.sqlite"),
            &temp.path().join("identity-pilot.sqlite"),
            1700000001000,
        )
        .unwrap();
        assert_eq!(summary.source_files, 1);
        assert_eq!(summary.verified_files, 1);
        assert_eq!(summary.restorable_files, 0);
        assert_eq!(summary.imported_restorable_files, 0);
        assert_eq!(summary.archived_only_files, 1);
        assert!(!summary.available_to_rust_restore_ui);
        assert!(!temp.path().join("identity.sqlite").exists());
    }

    #[test]
    fn model_and_fallback_timeouts_follow_node_inheritance() {
        let imported = super::map_core_runtime_config(&serde_json::json!({
            "api": {
                "baseUrl":"https://model.invalid/v1",
                "apiKey":"primary-key",
                "model":"primary",
                "timeoutMs":60000,
                "fallback":{"enabled":true,"model":"backup"}
            }
        }))
        .unwrap();
        assert_eq!(imported.model_timeout_ms, Some(60_000));
        assert_eq!(imported.model_fallback.unwrap().timeout_ms, 60_000);

        let explicit = super::map_core_runtime_config(&serde_json::json!({
            "api": {
                "baseUrl":"https://model.invalid/v1",
                "model":"primary",
                "timeoutMs":60000,
                "fallback":{"enabled":true,"model":"backup","timeoutMs":45000}
            }
        }))
        .unwrap();
        assert_eq!(explicit.model_fallback.unwrap().timeout_ms, 45_000);
    }

    #[test]
    fn model_fallback_is_counted_and_only_compatible_settings_clear_the_gate() {
        let compatible = serde_json::json!({
            "api": {
                "baseUrl":"https://model.invalid/v1",
                "timeoutMs":60000,
                "fallback":{"enabled":true,"model":"backup"}
            }
        });
        let enabled = effective_node_enabled_features(&compatible);
        assert!(enabled.contains(&"modelFallback".to_owned()));
        assert!(
            super::unmigrated_node_features(&compatible, &["modelFallback".to_owned()]).is_empty()
        );

        let custom_body = serde_json::json!({
            "api": {
                "baseUrl":"https://model.invalid/v1",
                "extraBody":{"custom_mode":true},
                "fallback":{"enabled":true,"model":"backup"}
            }
        });
        assert!(
            super::unmigrated_node_features(&custom_body, &["modelFallback".to_owned()])
                .contains(&"modelFallback".to_owned())
        );

        let unsupported_timeout = serde_json::json!({
            "api": {
                "baseUrl":"https://model.invalid/v1",
                "timeoutMs":360000,
                "fallback":{"enabled":true,"model":"backup"}
            }
        });
        assert!(super::unmigrated_node_features(
            &unsupported_timeout,
            &["modelFallback".to_owned()]
        )
        .contains(&"modelFallback".to_owned()));

        let disabled = serde_json::json!({
            "api":{"fallback":{"enabled":false,"model":"backup"}}
        });
        assert!(!effective_node_enabled_features(&disabled).contains(&"modelFallback".to_owned()));
    }

    #[test]
    fn incoming_friend_request_has_an_independent_runtime_and_import_mapping() {
        let config = serde_json::json!({
            "identityPilot": {
                "incomingFriendRequest": {
                    "enabled": true,
                    "autoWhitelist": false,
                    "maxPending": 37
                },
                "friendProposal": {"ownerUin":"123456789"}
            }
        });
        let enabled = effective_node_enabled_features(&config);
        assert!(enabled.contains(&"incomingFriendRequest".to_owned()));
        assert!(
            super::unmigrated_node_features(&config, &["incomingFriendRequest".to_owned()])
                .is_empty()
        );

        let imported = super::map_core_runtime_config(&config).unwrap();
        assert_eq!(imported.identity_pilot_enabled, Some(true));
        assert_eq!(imported.incoming_friend_request_enabled, Some(true));
        assert_eq!(imported.incoming_friend_auto_whitelist, Some(false));
        assert_eq!(imported.incoming_friend_max_pending, Some(37));
        assert_eq!(imported.identity_owner_uin.as_deref(), Some("123456789"));
    }

    #[test]
    fn memory_handoff_has_an_independent_runtime_and_import_mapping() {
        let config = serde_json::json!({
            "memory": {
                "handoffEnabled": true,
                "handoffTtlMinutes": 90,
                "handoffMaxChars": 6000
            },
            "tokenSaver": {"mode":"off"}
        });
        let enabled = effective_node_enabled_features(&config);
        assert!(enabled.contains(&"memoryHandoff".to_owned()));
        assert!(super::unmigrated_node_features(&config, &["memoryHandoff".to_owned()]).is_empty());

        let imported = super::map_core_runtime_config(&config).unwrap();
        assert_eq!(imported.memory_handoff_enabled, Some(true));
        assert_eq!(imported.memory_handoff_ttl_minutes, Some(90));
        assert_eq!(imported.memory_handoff_max_chars, Some(6000));

        let disabled = serde_json::json!({"memory":{"handoffEnabled":false}});
        assert!(!effective_node_enabled_features(&disabled).contains(&"memoryHandoff".into()));
    }

    #[test]
    fn memory_consolidation_maps_shared_chat_model_but_blocks_unimplemented_override() {
        let compatible = serde_json::json!({"memory":{"useChatModel":true}});
        assert!(
            super::unmigrated_node_features(&compatible, &["memoryConsolidate".to_owned()])
                .is_empty()
        );

        // Node defaults to the chat model when the preference is absent.
        let defaulted = serde_json::json!({"memory":{"provider":"legacy","model":"ignored"}});
        assert!(
            super::unmigrated_node_features(&defaulted, &["memoryConsolidate".to_owned()])
                .is_empty()
        );

        let dedicated_model = serde_json::json!({
            "memory":{"useChatModel":false,"provider":"memory-provider","model":"memory-model"}
        });
        assert_eq!(
            super::unmigrated_node_features(&dedicated_model, &["memoryConsolidate".to_owned()]),
            ["memoryConsolidate"]
        );
    }

    #[test]
    fn base_identity_maps_only_when_friend_proposals_are_disabled() {
        let compatible = serde_json::json!({
            "identityPilot": {
                "enabled": true,
                "friendProposal": {"enabled": false},
                "incomingFriendRequest": {"enabled": true}
            }
        });
        assert!(
            super::unmigrated_node_features(&compatible, &["identityPilot".to_owned()]).is_empty()
        );
        assert_eq!(
            super::map_core_runtime_config(&compatible)
                .unwrap()
                .identity_pilot_enabled,
            Some(true)
        );

        let proposal_enabled = serde_json::json!({
            "identityPilot": {"friendProposal":{"enabled":true}}
        });
        assert_eq!(
            super::unmigrated_node_features(&proposal_enabled, &["identityPilot".to_owned()]),
            ["identityPilot"]
        );
    }

    #[test]
    fn dedicated_vision_route_maps_image_input_only_when_fully_importable() {
        let enabled = ["visionInput".to_owned(), "visionFallback".to_owned()];
        let compatible = serde_json::json!({
            "api": {
                "vision": true,
                "visionModel":{"enabled":true,"provider":"vision-provider","model":"vision-model"}
            },
            "providers":[{"id":"vision-provider","baseURL":"https://vision.invalid/v1"}],
            "providerKeys":{"vision-provider":"vision-secret"}
        });
        assert!(super::unmigrated_node_features(&compatible, &enabled).is_empty());

        for patch in [
            serde_json::json!({"models":["vision-model", "second-vision-model"]}),
            serde_json::json!({"models":["second-vision-model"], "balanceFrom":"2099-01-01T00:00:00+08:00"}),
            serde_json::json!({"models":["second-vision-model"], "balanceFrom":"invalid"}),
            serde_json::json!({"thinking":"off"}),
            serde_json::json!({"thinking":"on"}),
        ] {
            let mut changed = compatible.clone();
            changed["api"]["visionModel"]
                .as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            assert_eq!(
                super::unmigrated_node_features(&changed, &enabled),
                enabled,
                "unimplemented vision pool/thinking overrides must fail closed"
            );
        }
        let mut duplicate_only = compatible.clone();
        duplicate_only["api"]["visionModel"]["models"] =
            serde_json::json!([" vision-model ", "", "vision-model"]);
        duplicate_only["api"]["visionModel"]["thinking"] = "auto".into();
        assert!(super::unmigrated_node_features(&duplicate_only, &enabled).is_empty());

        for incompatible in [
            serde_json::json!({
                "api":{"vision":false,"visionModel":{"enabled":true,"provider":"p","model":"m","baseUrl":"https://vision.invalid/v1","apiKey":"key"}}
            }),
            serde_json::json!({
                "api":{"visionModel":{"enabled":true,"provider":"p","model":"m","baseUrl":"https://vision.invalid/v1"}}
            }),
            serde_json::json!({
                "api":{"visionModel":{"enabled":true,"provider":"p","model":"m","baseUrl":"https://user:pass@vision.invalid/v1","apiKey":"key"}}
            }),
            serde_json::json!({
                "api":{"visionModel":{"enabled":true,"provider":"p","model":"m","baseUrl":"https://vision.invalid/v1","apiKey":"******"}}
            }),
            serde_json::json!({
                "api":{"visionModel":{"enabled":false,"provider":"p","model":"m","baseUrl":"https://vision.invalid/v1","apiKey":"key"}}
            }),
        ] {
            assert_eq!(
                super::unmigrated_node_features(&incompatible, &enabled),
                enabled
            );
        }

        let input_only = ["visionInput".to_owned()];
        assert_eq!(
            super::unmigrated_node_features(
                &serde_json::json!({"api":{"vision":true}}),
                &input_only
            ),
            input_only
        );
    }

    #[test]
    fn imports_maximum_per_chat_cooldown_from_chat_metadata_and_people() {
        let temp = tempfile::tempdir().unwrap();
        let memory = temp.path().join("memory");
        write_json(
            &memory.join("group_123/_meta.json"),
            serde_json::json!({"lastConsolidatedAt": 100}),
        );
        write_json(
            &memory.join("group_123/member.json"),
            serde_json::json!({
                "lastConsolidatedAt": "125",
                "impressions": [{"text":"private content must not be reported"}]
            }),
        );
        write_json(
            &memory.join("people/123.json"),
            serde_json::json!({
                "userId": "123",
                "updatedAt": 125,
                "lastConsolidatedAt": 150,
                "sourceChatKeys": ["group:123", "group:456"],
                "impressions": [{"sourceChatKey":"private:9"}]
            }),
        );
        write_json(
            &memory.join("people/person-empty.json"),
            serde_json::json!({"lastConsolidatedAt": 999, "impressions": []}),
        );
        let target = temp.path().join("cooldowns.sqlite");

        let identity_source = temp.path().join("identity-pilot.sqlite");
        let summary =
            import_memory_consolidation_cooldowns(&memory, &identity_source, &target, true)
                .unwrap();
        assert_eq!(summary.source_chat_metadata_files, 1);
        assert_eq!(summary.source_person_files, 3);
        assert_eq!(summary.imported_chat_cooldowns, 3);
        assert_eq!(summary.imported_person_cooldowns, 1);
        assert_eq!(summary.imported_person_updated_rows, 1);
        assert_eq!(summary.verified_chat_rows, 3);
        assert_eq!(summary.verified_person_cooldown_rows, 1);
        assert_eq!(summary.verified_person_updated_rows, 1);
        assert_eq!(summary.verified_samples, 3);

        let store = crate::application::memory_consolidation::MemoryConsolidationStore::open_existing_read_only(&target).unwrap();
        for (chat, expected) in [("group:123", 150), ("group:456", 150), ("private:9", 150)] {
            assert_eq!(
                store.chat_run_state(chat).unwrap().unwrap().last_attempt_at,
                expected
            );
        }
        assert!(store.chat_run_state("group:789").unwrap().is_none());
        drop(store);
        let second =
            import_memory_consolidation_cooldowns(&memory, &identity_source, &target, true)
                .unwrap();
        assert_eq!(second.imported_chat_cooldowns, 3);
        let store = crate::application::memory_consolidation::MemoryConsolidationStore::open_existing_read_only(&target).unwrap();
        assert_eq!(store.cooldown_count().unwrap(), 3);
        assert_eq!(store.latest_person_cooldown(&["123".into()]).unwrap(), 150);
        assert_eq!(store.person_updated_at("123").unwrap(), 125);
    }

    #[test]
    fn disabled_or_missing_memory_does_not_create_cooldown_database() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing");
        let target = temp.path().join("cooldowns.sqlite");
        assert_eq!(
            import_memory_consolidation_cooldowns(
                &missing,
                &temp.path().join("identity-pilot.sqlite"),
                &target,
                true,
            )
            .unwrap(),
            Default::default()
        );
        assert_eq!(
            import_memory_consolidation_cooldowns(
                temp.path(),
                &temp.path().join("identity-pilot.sqlite"),
                &target,
                false,
            )
            .unwrap(),
            Default::default()
        );
        assert!(!target.exists());
    }

    #[test]
    fn malformed_cooldown_metadata_aborts_without_creating_target() {
        let temp = tempfile::tempdir().unwrap();
        let memory = temp.path().join("memory");
        write_json(
            &memory.join("private_77/_meta.json"),
            serde_json::json!({"lastConsolidatedAt":"not-a-timestamp"}),
        );
        let target = temp.path().join("cooldowns.sqlite");
        assert!(import_memory_consolidation_cooldowns(
            &memory,
            &temp.path().join("identity-pilot.sqlite"),
            &target,
            true
        )
        .is_err());
        assert!(!target.exists());
    }

    #[test]
    fn feature_audit_uses_effective_node_defaults_and_forced_stable_modules() {
        let effective = effective_node_enabled_features(&serde_json::json!({}));
        assert_eq!(
            effective,
            [
                "visionInput",
                "webSearch",
                "followUp",
                "selfWake",
                "identityPilot",
                "incidentPilot",
                "sticker",
                "stickerCollect",
                "memoryConsolidate",
                "memoryHandoff"
            ]
        );

        let explicitly_disabled = effective_node_enabled_features(&serde_json::json!({
            "api": {"vision": false},
            "webSearch": {"enabled": false},
            "asr": {"enabled": false},
            "proactive": {"followUpEnabled": false, "selfWakeEnabled": false},
            "sticker": {"enabled": false, "collectEnabled": false},
            "memory": {"consolidateEnabled": false, "handoffEnabled": false},
            "identityPilot": {
                "enabled": false,
                "incomingFriendRequest": {"enabled": false}
            },
            "incidentPilot": {"enabled": false}
        }));
        assert_eq!(explicitly_disabled, ["identityPilot", "incidentPilot"]);
    }

    #[test]
    fn proactive_features_are_mapped_only_in_the_runtime_and_data_guarded_path() {
        let config = serde_json::json!({
            "proactive":{"enabled":true,"followUpEnabled":true,"selfWakeEnabled":true}
        });
        let enabled = effective_node_enabled_features(&config);
        let proactive = ["proactive", "followUp", "selfWake"]
            .map(str::to_owned)
            .to_vec();
        assert!(proactive.iter().all(|feature| enabled.contains(feature)));
        assert!(super::unmigrated_node_features(&config, &proactive).is_empty());

        // Other unfinished Node-only behavior must still block the import.
        assert_eq!(
            super::unmigrated_node_features(&config, &["proactive".into(), "pacing".into()]),
            ["pacing"]
        );
    }

    #[test]
    fn group_invite_behavior_is_mapped_but_real_qq_acceptance_stays_a_separate_gate() {
        let config = serde_json::json!({
            "groupInvite": {
                "enabled": true,
                "friendsOnly": true,
                "autoWhitelist": true,
                "notifyOwner": true
            },
            "admin": {"ownerUin": "123456"},
            "allow": {"groups": ["100"], "private": ["200"]},
            "deny": {"groups": [], "private": []}
        });
        let enabled = effective_node_enabled_features(&config);
        assert!(enabled.contains(&"groupInvite".to_owned()));
        assert!(
            !super::unmigrated_node_features(&config, &enabled).contains(&"groupInvite".to_owned())
        );

        let imported = super::map_core_runtime_config(&config).unwrap();
        assert_eq!(imported.group_invite_enabled, Some(true));
        assert_eq!(imported.group_invite_friends_only, Some(true));
        assert_eq!(imported.group_invite_auto_whitelist, Some(true));
        assert_eq!(imported.group_invite_notify_owner, Some(true));
        assert_eq!(imported.allow_groups.as_deref(), Some("100"));
        assert_eq!(imported.allow_private.as_deref(), Some("200"));
    }

    #[test]
    fn incident_pilot_behavior_is_mapped_but_real_alert_delivery_is_separate() {
        let config = serde_json::json!({
            "incidentPilot": {
                "enabled": true,
                "graduated": false,
                "ownerUin": "123456",
                "notifyWarnings": false,
                "duplicateWindowMinutes": 30,
                "retentionDays": 180,
                "unknownWritesBlockChat": true
            }
        });
        let enabled = effective_node_enabled_features(&config);
        assert!(enabled.contains(&"incidentPilot".to_owned()));
        assert!(!super::unmigrated_node_features(&config, &enabled)
            .contains(&"incidentPilot".to_owned()));

        let imported = super::map_core_runtime_config(&config).unwrap();
        assert_eq!(imported.incident_pilot_enabled, Some(true));
        assert_eq!(imported.incident_pilot_graduated, Some(false));
        assert_eq!(imported.incident_owner_uin.as_deref(), Some("123456"));
        assert_eq!(imported.incident_notify_warnings, Some(false));
        assert_eq!(imported.incident_duplicate_window_minutes, Some(30));
        assert_eq!(imported.incident_retention_days, Some(180));
        assert_eq!(imported.incident_unknown_writes_block_chat, Some(true));
    }

    #[test]
    fn relationship_pilot_behavior_is_mapped_but_live_acceptance_stays_separate() {
        let config = serde_json::json!({
            "identityPilot":{"enabled":true},
            "relationshipPilot":{
                "enabled":true,
                "graduated":false,
                "model":"",
                "minNewMessages":8,
                "minBatchMessages":3,
                "minBatchAgeMinutes":360,
                "maxEvidenceMessages":32,
                "maxPeoplePerTurn":2,
                "maxEvaluationsPerDay":20,
                "familiarityRefreshMinutes":5,
                "frictionHalfLifeHours":48,
                "maxMemoryContext":6
            }
        });
        let enabled = effective_node_enabled_features(&config);
        assert!(enabled.contains(&"relationshipPilot".to_owned()));
        assert!(!super::unmigrated_node_features(&config, &enabled)
            .contains(&"relationshipPilot".to_owned()));

        let imported = super::map_core_runtime_config(&config).unwrap();
        let relationship = imported.relationship_pilot.unwrap();
        assert!(relationship.enabled);
        assert!(!relationship.graduated);
        assert_eq!(relationship.min_new_messages, 8);
        assert_eq!(relationship.max_evaluations_per_day, 20);
    }

    #[test]
    fn feature_audit_inventories_unlisted_behavior_modes_and_blocks_them_when_enabled() {
        for (key, feature) in [
            ("pacing", "pacing"),
            ("toolSchedulerPilot", "toolSchedulerPilot"),
            ("slangPilot", "slangPilot"),
        ] {
            let config = serde_json::json!({(key): {"enabled": true}});
            let enabled = effective_node_enabled_features(&config);
            assert!(
                enabled.contains(&feature.to_owned()),
                "{feature} not inventoried"
            );
            assert!(
                super::unmigrated_node_features(&config, &enabled).contains(&feature.to_owned()),
                "{feature} must remain blocked"
            );
        }

        let disabled = effective_node_enabled_features(&serde_json::json!({
            "pacing":{"enabled":false},
            "toolSchedulerPilot":{"enabled":false},
            "slangPilot":{"enabled":false}
        }));
        assert!(!disabled.contains(&"pacing".to_owned()));
        assert!(!disabled.contains(&"toolSchedulerPilot".to_owned()));
        assert!(!disabled.contains(&"slangPilot".to_owned()));
    }

    #[test]
    fn only_the_configured_supported_bing_search_path_clears_its_active_blocker() {
        let enabled = vec!["webSearch".to_owned()];
        for config in [
            serde_json::json!({"webSearch":{"enabled":true}}),
            serde_json::json!({"webSearch":{"enabled":true,"provider":"BING","searchUrl":"https://cn.bing.com/search"}}),
        ] {
            assert!(super::unmigrated_node_features(&config, &enabled).is_empty());
        }
        for config in [
            serde_json::json!({"webSearch":{"enabled":true,"provider":"deepseek"}}),
            serde_json::json!({"webSearch":{"enabled":true,"provider":"custom:private"}}),
            serde_json::json!({"webSearch":{"enabled":true,"provider":"bing","searchUrl":"https://example.com/search"}}),
            serde_json::json!({"webSearch":{"enabled":true,"provider":"bing","searchUrl":"http://cn.bing.com/search"}}),
        ] {
            assert_eq!(
                super::unmigrated_node_features(&config, &enabled),
                ["webSearch"]
            );
        }
    }

    #[test]
    fn configured_node_recall_notifications_are_inventoried_and_mapped_only_for_valid_owner() {
        let config = serde_json::json!({"admin":{"ownerUin":"123456"}});
        let enabled = effective_node_enabled_features(&config);
        assert!(enabled.contains(&"recallNotifications".into()));
        assert!(!super::unmigrated_node_features(&config, &enabled)
            .contains(&"recallNotifications".into()));

        let disabled = serde_json::json!({"admin":{"ownerUin":"  "}});
        assert!(!effective_node_enabled_features(&disabled).contains(&"recallNotifications".into()));

        for invalid_owner in ["1234", "1234567890123456", "12x456"] {
            let invalid = serde_json::json!({"admin":{"ownerUin":invalid_owner}});
            let enabled = effective_node_enabled_features(&invalid);
            assert!(!enabled.contains(&"recallNotifications".into()));
            assert!(
                super::unmigrated_node_features(&invalid, &["recallNotifications".into()])
                    .contains(&"recallNotifications".into())
            );
        }
    }

    #[test]
    fn feature_audit_blocks_enabled_threaded_modes_using_node_unified_mode_rules() {
        let global_lifecycle = effective_node_enabled_features(&serde_json::json!({
            "conversation": {"mode":"lifecycle"}
        }));
        assert!(global_lifecycle.contains(&"conversationThreads".into()));

        let group_threaded = effective_node_enabled_features(&serde_json::json!({
            "conversation": {
                "mode":"legacy",
                "unifiedMode":false,
                "groupModes":{"123":"threaded"}
            }
        }));
        assert!(group_threaded.contains(&"conversationThreads".into()));

        let ignored_group_override = effective_node_enabled_features(&serde_json::json!({
            "conversation": {
                "mode":"legacy",
                "unifiedMode":true,
                "groupModes":{"123":"lifecycle"}
            }
        }));
        assert!(!ignored_group_override.contains(&"conversationThreads".into()));

        let global_threaded_even_with_group_override =
            effective_node_enabled_features(&serde_json::json!({
                "conversation": {
                    "mode":"threaded",
                    "unifiedMode":false,
                    "groupModes":{"123":"legacy"}
                }
            }));
        assert!(global_threaded_even_with_group_override.contains(&"conversationThreads".into()));

        let enabled_group_invite = effective_node_enabled_features(&serde_json::json!({
            "groupInvite":{"enabled":true}
        }));
        assert!(enabled_group_invite.contains(&"groupInvite".into()));
    }

    #[test]
    fn multimodal_context_maps_legacy_handoff_but_keeps_lifecycle_rollover_blocked() {
        let legacy = serde_json::json!({
            "multimodalContextPilot":{"enabled":true},
            "conversation":{"mode":"legacy","unifiedMode":true}
        });
        let enabled = effective_node_enabled_features(&legacy);
        assert!(enabled.contains(&"multimodalContext".into()));
        assert!(!super::unmigrated_node_features(&legacy, &enabled)
            .contains(&"multimodalContext".into()));

        let global_lifecycle = serde_json::json!({
            "multimodalContextPilot":{"enabled":true},
            "conversation":{"mode":"lifecycle"}
        });
        let enabled = effective_node_enabled_features(&global_lifecycle);
        let unmapped = super::unmigrated_node_features(&global_lifecycle, &enabled);
        assert!(unmapped.contains(&"multimodalContext".into()));
        assert!(unmapped.contains(&"conversationThreads".into()));

        let group_lifecycle = serde_json::json!({
            "multimodalContextPilot":{"enabled":true},
            "conversation":{
                "mode":"legacy",
                "unifiedMode":false,
                "groupModes":{"123":"lifecycle"}
            }
        });
        let enabled = effective_node_enabled_features(&group_lifecycle);
        let unmapped = super::unmigrated_node_features(&group_lifecycle, &enabled);
        assert!(unmapped.contains(&"multimodalContext".into()));
        assert!(unmapped.contains(&"conversationThreads".into()));
    }

    #[test]
    fn maps_node_daily_moments_defaults_and_operational_configuration() {
        let imported = super::map_core_runtime_config(&serde_json::json!({
            "persona": {
                "botName":"凛",
                "selfNickname":"凛酱",
                "selfAppearance":"银发",
                "roleText":"喜欢观察生活细节",
                "customRules":"不虚构经历",
                "participation":"偶尔分享想法"
            },
            "dailyMoments": {
                "enabled": true,
                "hour": 6,
                "minute": 45,
                "scheduleWindows": [{"start":"18:00","end":"20:00","count":2}],
                "intervalDays": 3,
                "startupCatchup": false,
                "minMessagesPerGroup": 5,
                "maxGroups": 9,
                "maxMessagesPerGroup": 120,
                "maxPromptChars": 180000,
                "allowImages": false,
                "maxImages": 0,
                "visibility": 64,
                "targetUins": ["12345678", 87654321],
                "maxResearchCalls": 6,
                "maxRounds": 11
            },
            "sticker": {
                "enabled": true,
                "collectEnabled": false,
                "autoCollect": true,
                "maxCollectPerHour": 17,
                "promptMaxStickers": 24,
                "encourage": 3
            },
            "memory": {
                "consolidateEnabled": false,
                "consolidateMinImpressions": 7,
                "maxImpressionsPerMember": 8,
                "discoverMinMessages": 25,
                "discoverMaxMembers": 4,
                "consolidateMinIntervalMs": 3600000
            }
        }))
        .unwrap();
        assert_eq!(imported.memory_consolidate_enabled, Some(false));
        assert_eq!(imported.memory_consolidate_min_impressions, Some(7));
        assert_eq!(imported.memory_consolidate_max_per_member, Some(8));
        assert_eq!(imported.memory_consolidate_discover_min_messages, Some(25));
        assert_eq!(imported.memory_consolidate_discover_max_members, Some(4));
        assert_eq!(imported.memory_consolidate_min_interval_ms, Some(3_600_000));
        assert_eq!(imported.sticker_enabled, Some(true));
        assert_eq!(imported.sticker_collect_enabled, Some(false));
        assert_eq!(imported.sticker_auto_collect_enabled, Some(true));
        assert_eq!(imported.sticker_save_to_qq_favorites, Some(true));
        assert_eq!(imported.sticker_max_collect_per_hour, Some(17));
        assert_eq!(imported.sticker_prompt_max_stickers, Some(24));
        assert_eq!(imported.sticker_encourage, Some(3));
        let settings = imported.daily_moments.unwrap();
        assert_eq!(settings.hour, 6);
        assert_eq!(settings.minute, 45);
        assert_eq!(settings.schedule_windows.as_ref().unwrap()[0].count, 2);
        assert_eq!(settings.interval_days, 3);
        assert!(!settings.startup_catchup);
        assert_eq!(settings.min_messages_per_group, 5);
        assert_eq!(settings.max_groups, 9);
        assert_eq!(settings.max_messages_per_group, 120);
        assert_eq!(settings.max_prompt_chars, 180_000);
        assert!(!settings.allow_images);
        assert_eq!(settings.max_images, 0);
        assert_eq!(settings.visibility, 64);
        assert_eq!(settings.target_uins, vec!["12345678", "87654321"]);
        assert_eq!(settings.max_research_calls, 6);
        assert_eq!(settings.max_rounds, 11);
        let persona = imported.daily_moments_persona.unwrap();
        assert_eq!(persona.bot_name, "凛");
        assert_eq!(persona.self_nickname, "凛酱");
        assert_eq!(persona.self_appearance, "银发");
        assert_eq!(persona.role_text, "喜欢观察生活细节");
        assert_eq!(persona.custom_rules, "不虚构经历");
        assert_eq!(persona.participation, "偶尔分享想法");

        let defaults = super::map_core_runtime_config(&serde_json::json!({"dailyMoments": {}}))
            .unwrap()
            .daily_moments
            .unwrap();
        assert_eq!(
            defaults,
            crate::daily_moments::DailyMomentsSettings::default()
        );

        let sticker_defaults = super::map_core_runtime_config(&serde_json::json!({})).unwrap();
        assert_eq!(sticker_defaults.memory_consolidate_enabled, Some(true));
        assert_eq!(
            sticker_defaults.memory_consolidate_min_interval_ms,
            Some(21_600_000)
        );
        assert_eq!(sticker_defaults.sticker_enabled, Some(true));
        assert_eq!(sticker_defaults.sticker_collect_enabled, Some(true));
        assert_eq!(sticker_defaults.sticker_auto_collect_enabled, Some(true));
        assert_eq!(sticker_defaults.sticker_save_to_qq_favorites, Some(true));
        assert_eq!(sticker_defaults.sticker_max_collect_per_hour, Some(10));
        assert_eq!(sticker_defaults.send_max_per_minute, Some(80));
        assert_eq!(sticker_defaults.send_max_per_hour, Some(500));
        assert_eq!(sticker_defaults.send_min_gap_ms, Some(1_000));
        assert_eq!(sticker_defaults.send_max_gap_ms, Some(3_000));
        assert_eq!(sticker_defaults.send_by_length_ms, Some(20));
        assert_eq!(sticker_defaults.send_hard_split_at, Some(4_000));

        let imported_send_rates = super::map_core_runtime_config(&serde_json::json!({
            "send": {
                "maxPerMinute": 24,
                "maxPerHour": 240,
                "minGapMs": 250,
                "maxGapMs": 900,
                "byLengthMs": 8,
                "hardSplitAt": 1_800
            }
        }))
        .unwrap();
        assert_eq!(imported_send_rates.send_max_per_minute, Some(24));
        assert_eq!(imported_send_rates.send_max_per_hour, Some(240));
        assert_eq!(imported_send_rates.send_min_gap_ms, Some(250));
        assert_eq!(imported_send_rates.send_max_gap_ms, Some(900));
        assert_eq!(imported_send_rates.send_by_length_ms, Some(8));
        assert_eq!(imported_send_rates.send_hard_split_at, Some(1_800));
    }

    #[test]
    fn daily_moments_mapping_respects_node_token_saver_round_caps() {
        for mode in ["balanced", "aggressive"] {
            let config = serde_json::json!({
                "dailyMoments": {"enabled": true, "maxRounds": 12},
                "tokenSaver": {"mode": mode}
            });
            assert!(
                super::unmigrated_node_features(&config, &["dailyMoments".to_owned()])
                    .contains(&"dailyMoments".to_owned())
            );
        }

        for mode in ["off", "unknown"] {
            let config = serde_json::json!({
                "dailyMoments": {"enabled": true, "maxRounds": 12},
                "tokenSaver": {"mode": mode}
            });
            assert!(
                super::unmigrated_node_features(&config, &["dailyMoments".to_owned()]).is_empty()
            );
        }
    }

    #[test]
    fn sticker_features_have_static_mappings_without_claiming_live_acceptance() {
        let node_configuration = serde_json::json!({
            "sticker": {
                "enabled": true,
                "collectEnabled": true,
                "autoCollect": true,
                "saveToQqFavorites": true,
                "maxCollectPerHour": 10,
                "promptMaxStickers": 10,
                "encourage": 3
            }
        });
        let enabled = super::effective_node_enabled_features(&node_configuration);
        assert!(enabled.contains(&"sticker".to_owned()));
        assert!(enabled.contains(&"stickerCollect".to_owned()));
        assert!(super::unmigrated_node_features(
            &node_configuration,
            &["sticker".to_owned(), "stickerCollect".to_owned()]
        )
        .is_empty());

        let imported = super::map_core_runtime_config(&node_configuration).unwrap();
        assert_eq!(imported.sticker_enabled, Some(true));
        assert_eq!(imported.sticker_collect_enabled, Some(true));
        assert_eq!(imported.sticker_auto_collect_enabled, Some(true));
        assert_eq!(imported.sticker_save_to_qq_favorites, Some(true));
        assert_eq!(imported.sticker_max_collect_per_hour, Some(10));
        assert_eq!(imported.sticker_prompt_max_stickers, Some(10));
        assert_eq!(imported.sticker_encourage, Some(3));
    }

    #[test]
    fn maps_qzone_interaction_runtime_controls_without_enabling_missing_feature() {
        let imported = super::map_core_runtime_config(&serde_json::json!({
            "qzoneInteractions": {
                "enabled": true,
                "startupCatchup": true,
                "activeHours": {"start":"22:00", "end":"06:30"},
                "feedIntervalMinutes": 45,
                "replyIntervalMinutes": 7,
                "feedFetchCount": 40,
                "maxLikesPerRun": 4,
                "actionDelayMinMs": 800,
                "actionDelayMaxMs": 1900
            }
        }))
        .unwrap();
        let settings = imported.qzone_interactions.unwrap();
        assert!(settings.enabled);
        assert!(settings.startup_catchup);
        assert_eq!(settings.active_hours.as_ref().unwrap().start, "22:00");
        assert_eq!(settings.feed_interval_minutes, 45);
        assert_eq!(settings.reply_interval_minutes, 7);
        assert_eq!(settings.feed_fetch_count, 40);
        assert_eq!(settings.max_likes_per_run, 4);
        assert_eq!(settings.action_delay_min_ms, 800);
        assert_eq!(settings.action_delay_max_ms, 1900);
        assert_eq!(settings.decision_input_budget_tokens, 151_808);

        let defaults = super::map_core_runtime_config(&serde_json::json!({}))
            .unwrap()
            .qzone_interactions;
        assert!(defaults.is_none());
    }

    #[test]
    fn qzone_input_budget_matches_node_context_and_token_saver_caps() {
        for (mode, max_run, context, expected) in [
            ("off", 100_000, 1_000_000, 91_808),
            ("balanced", 100_000, 1_000_000, 71_808),
            ("aggressive", 100_000, 1_000_000, 41_808),
            ("off", 160_000, 32_000, 23_808),
        ] {
            let settings = super::map_core_runtime_config(&serde_json::json!({
                "api":{"maxRunTokens":max_run,"contextWindowTokens":context},
                "tokenSaver":{"mode":mode},
                "qzoneInteractions":{"enabled":true}
            }))
            .unwrap()
            .qzone_interactions
            .unwrap();
            assert_eq!(
                settings.decision_input_budget_tokens, expected,
                "mode={mode}"
            );
        }
    }

    #[test]
    fn qzone_feature_maps_only_when_node_model_overrides_and_persona_are_preserved() {
        let compatible = serde_json::json!({
            "api": {
                "baseUrl":"https://model.example/v1",
                "maxRunTokens":100_000,
                "contextWindowTokens":1_000_000,
                "thinking":"off",
                "thinkingByService":{"model.example":{"write":"on","default":"on"}},
                "extraBody":{}
            },
            "tokenSaver":{"mode":"off"},
            "persona":{"roleText":"完整人设","selfAppearance":"外貌"},
            "qzoneInteractions":{"enabled":true}
        });
        assert!(
            super::unmigrated_node_features(&compatible, &["qzoneInteractions".to_owned()])
                .is_empty()
        );

        let custom_thinking = serde_json::json!({
            "api": {
                "provider":"local",
                "baseUrl":"http://127.0.0.1:8789/v1",
                "thinking":"off",
                "thinkingParams":{"off":{"thinking":{"type":"disabled"}}}
            },
            "providers":[{"id":"local","preset":"custom"}],
            "qzoneInteractions":{"enabled":true}
        });
        assert!(super::unmigrated_node_features(
            &custom_thinking,
            &["qzoneInteractions".to_owned()]
        )
        .is_empty());
        let imported = super::map_core_runtime_config(&custom_thinking)
            .unwrap()
            .qzone_interactions
            .unwrap();
        assert_eq!(
            imported.thinking_request_patch,
            Some(serde_json::json!({"thinking":{"type":"disabled"}}))
        );

        for incompatible in [
            serde_json::json!({
                "api":{"extraBody":{"reasoning":{"enabled":false}}},
                "qzoneInteractions":{"enabled":true}
            }),
            serde_json::json!({
                "api":{"baseUrl":"https://model.example/v1","thinkingByService":{"model.example":{"write":"off"}}},
                "qzoneInteractions":{"enabled":true}
            }),
            serde_json::json!({
                "persona":{"roleText":"字".repeat(20_001)},
                "qzoneInteractions":{"enabled":true}
            }),
        ] {
            assert_eq!(
                super::unmigrated_node_features(&incompatible, &["qzoneInteractions".to_owned()]),
                ["qzoneInteractions".to_owned()]
            );
        }
    }

    #[test]
    fn maps_proactive_runtime_settings_without_graduating_the_module() {
        let imported = super::map_core_runtime_config(&serde_json::json!({
            "maxConcurrentRuns": 5,
            "proactive": {
                "enabled": true,
                "graduated": true,
                "checkIntervalMinMs": 900000,
                "checkIntervalMaxMs": 2700000,
                "idleThresholdMs": 1200000,
                "probability": 0.4,
                "followUpEnabled": false,
                "selfWakeEnabled": true,
                "activeHours": {"windows":[{"start":"22:00","end":"06:00"},{"start":"bad","end":"09:00"}]}
            }
        }))
        .unwrap();
        assert_eq!(imported.max_concurrent_runs, Some(5));
        let settings = imported.proactive.unwrap();
        assert!(settings.enabled);
        assert!(
            !settings.graduated,
            "Node configuration cannot graduate Rust modules"
        );
        assert_eq!(settings.check_interval_min_ms, 900_000);
        assert_eq!(settings.check_interval_max_ms, 2_700_000);
        assert_eq!(settings.idle_threshold_ms, 1_200_000);
        assert_eq!(settings.probability, 0.4);
        assert!(!settings.follow_up_enabled);
        assert!(settings.self_wake_enabled);
        let hours = settings.active_hours.unwrap();
        assert_eq!(hours.len(), 1);
        assert_eq!(hours[0].start, "22:00");
        assert_eq!(hours[0].end, "06:00");

        let defaults = super::map_core_runtime_config(&serde_json::json!({}))
            .unwrap()
            .proactive
            .unwrap();
        assert_eq!(
            defaults,
            crate::application::proactive::ProactiveSettings::default()
        );
        assert_eq!(
            super::map_core_runtime_config(&serde_json::json!({}))
                .unwrap()
                .max_concurrent_runs,
            Some(2)
        );
    }

    #[test]
    fn maps_handoff_character_limit_and_node_token_saver_caps() {
        let cases = [
            (
                serde_json::json!({"memory":{"handoffMaxChars":8000}}),
                8_000,
            ),
            (
                serde_json::json!({"memory":{"handoffMaxChars":8000},"tokenSaver":{"mode":"balanced"}}),
                2_000,
            ),
            (
                serde_json::json!({"memory":{"handoffMaxChars":8000},"tokenSaver":{"mode":"aggressive"}}),
                1_200,
            ),
            (
                serde_json::json!({"memory":{"handoffMaxChars":100},"tokenSaver":{"mode":"off"}}),
                500,
            ),
            (serde_json::json!({}), 4_000),
        ];
        for (node_config, expected) in cases {
            let imported = super::map_core_runtime_config(&node_config).unwrap();
            assert_eq!(imported.memory_handoff_max_chars, Some(expected));
        }
        for (mode, expected) in [("off", 24), ("balanced", 5), ("aggressive", 3)] {
            let imported = super::map_core_runtime_config(&serde_json::json!({
                "sticker":{"promptMaxStickers":24},
                "tokenSaver":{"mode":mode}
            }))
            .unwrap();
            assert_eq!(imported.sticker_prompt_max_stickers, Some(expected));
        }
    }

    #[test]
    fn importer_maps_stable_identity_policy_not_stale_disk_flags() {
        let imported = super::map_core_runtime_config(&serde_json::json!({
            "api": {"maxRounds": 7},
            "webSearch":{"enabled":true,"provider":"bing","searchUrl":"https://cn.bing.com/search","maxResults":8},
            "groupInvite":{"enabled":true,"autoWhitelist":true,"friendsOnly":true,"notifyOwner":true},
            "admin":{"ownerUin":"998877"},
            "identityPilot": {
                "enabled": false,
                "graduated": false,
                "incomingFriendRequest": {"enabled": false}
            }
        }))
        .unwrap();
        assert_eq!(imported.identity_pilot_enabled, Some(true));
        assert_eq!(imported.identity_pilot_graduated, Some(true));
        assert_eq!(imported.web_search_enabled, Some(true));
        assert_eq!(imported.web_search_provider.as_deref(), Some("bing"));
        assert_eq!(imported.web_search_max_results, Some(8));
        assert_eq!(imported.model_max_rounds, Some(7));
        assert_eq!(imported.incoming_friend_request_enabled, Some(false));
        assert_eq!(imported.group_invite_enabled, Some(true));
        assert_eq!(imported.group_invite_auto_whitelist, Some(true));
        assert_eq!(imported.group_invite_friends_only, Some(true));
        assert_eq!(imported.group_invite_notify_owner, Some(true));
        assert_eq!(imported.group_invite_owner_uin.as_deref(), Some("998877"));
        assert_eq!(imported.admin_owner_uin.as_deref(), Some("998877"));
        assert_eq!(imported.incident_owner_uin, None);
    }

    #[test]
    fn importer_converts_global_and_group_legacy_slider_semantics() {
        let imported = super::map_core_runtime_config(&serde_json::json!({
            "store": {
                "contextSliderPos": 55,
                "randomPercent": 25,
                "contextTier": 3,
                "unifiedTier": false,
                "groupSliderPos": {"200": 10, "201": 55, "202": 95}
            }
        }))
        .unwrap();
        assert_eq!(imported.random_response_percent, Some(50.0));
        assert_eq!(imported.unified_tier, Some(false));
        let groups = imported.group_random_percent.unwrap();
        assert_eq!(groups["200"], 0.0);
        assert_eq!(groups["201"], 50.0);
        assert_eq!(groups["202"], 100.0);

        let tier_only = super::map_core_runtime_config(&serde_json::json!({
            "store": {"contextTier": 2, "randomPercent": 35}
        }))
        .unwrap();
        assert_eq!(tier_only.random_response_percent, Some(0.0));
    }
}
