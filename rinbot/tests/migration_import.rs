use clap::Parser;
use rinbot::{
    adapters::sqlite::SqliteStore,
    application::proactive::ProactiveStore,
    migration_audit::audit_directory,
    migration_import::{fingerprint_legacy_archive, import_stopped_node_directory, ImportError},
};
use rusqlite::Connection;

fn create_node_source(path: &std::path::Path) {
    std::fs::create_dir_all(path.join("sessions")).unwrap();
    std::fs::create_dir_all(path.join("memory/group_200")).unwrap();
    std::fs::create_dir_all(path.join("memory/private_100")).unwrap();
    std::fs::create_dir_all(path.join("memory/people")).unwrap();
    std::fs::create_dir_all(path.join("sticker-assets")).unwrap();
    std::fs::write(
        path.join("sticker-assets/test.gif"),
        b"GIF89a-fixture-image",
    )
    .unwrap();
    std::fs::write(
        path.join("memory/group_200/_handoff.json"),
        r#"{"version":1,"topic":"持续讨论","summary":"下一轮继续验证","facts":["已有证据"],"participantIds":["100"],"updatedAt":1700000000000,"expiresAt":4102444800000}"#,
    ).unwrap();
    std::fs::write(
        path.join("memory/group_200.json"),
        r#"{"activeTopic":"旧主题不能覆盖","pendingThought":"旧状态不能覆盖"}"#,
    )
    .unwrap();
    std::fs::write(
        path.join("memory/private_300.json"),
        r#"{"activeTopic":["待办主题",{"text":"补充主题"}],"pendingThought":{"summary":"下一步安排"}}"#,
    )
    .unwrap();
    std::fs::write(
        path.join("memory/private_100/_handoff.json"),
        r#"{"version":1,"topic":"已过期状态","summary":"不应恢复","updatedAt":1,"expiresAt":2}"#,
    )
    .unwrap();
    std::fs::write(
        path.join("memory/people/100.json"),
        r#"{"version":2,"userId":"100","name":"Alice","updatedAt":1700000005000,"sourceChatKeys":["private:100","group:200"],"impressions":[{"content":"喜欢徒步","createdAt":1700000000000,"lastObservedAt":1700000001000,"origin":"model","sourceChatKeys":["private:100","group:200"]},{"content":"常聊露营","createdAt":1700000002000,"sourceChatKeys":["group:200"]}]}"#,
    )
    .unwrap();
    std::fs::create_dir_all(path.join("memory/backups/consolidation/100")).unwrap();
    std::fs::write(
        path.join("memory/backups/consolidation/100/1700000003000-fixture.json"),
        r#"{"version":1,"reason":"consolidation","sourceChatKey":"group:200","backedUpAt":1700000003000,"person":{"version":2,"userId":"100","name":"Alice","sourceChatKeys":["group:200","private:100"],"impressions":[{"content":"Node 整理前快照印象","createdAt":1700000002000,"lastObservedAt":1700000002500,"origin":"model","sourceChatKeys":["group:200","private:100"]}]}}"#,
    )
    .unwrap();
    std::fs::write(
        path.join("config.json"),
        r#"{"api":{"baseUrl":"https://model.invalid/v1","apiKey":"","provider":"fixture-provider","model":"fixture-model","temperature":0.4,"vision":true,"visionModel":{"enabled":true,"provider":"fixture-vision","model":"vision-fixture","timeoutMs":55000},"fallback":{"enabled":true,"baseUrl":"https://backup.invalid/v1","apiKey":"fallback-fixture-secret","model":"backup-fixture-model","timeoutMs":45000}},"providers":[{"id":"fixture-provider","apiKey":"******"},{"id":"fixture-vision","baseURL":"https://vision.invalid/v1"}],"providerKeys":{"fixture-provider":"never-print-this","fixture-vision":"vision-fixture-secret"},"onebot":{"wsUrl":"ws://127.0.0.1:3001","httpUrl":"http://127.0.0.1:3000","accessToken":"fixture-token"},"persona":{"roleText":"中文角色","customRules":"保持简洁"},"allow":{"groups":["200"],"private":["100"]},"deny":{"groups":[],"private":[]},"blocklist":{"200":[300,"400"]},"store":{"randomPercent":15,"unifiedTier":false,"sliderMode":"probability","groupSliderPos":{"200":0,"201":42.5},"keywords":["测试","帮助"],"atCount":12,"keywordCount":8,"randomCount":5,"allCount":20,"batchLimit":10,"batchMaxChars":12000},"wakeDelayMinMs":1000,"wakeDelayMaxMs":2000,"maxBatchWaitMs":5000,"timeControl":{"enabled":false,"schedule":{"mode":"always","windows":[]},"overrides":{}},"identityPilot":{"enabled":true,"graduated":true,"incomingFriendRequest":{"enabled":true,"autoWhitelist":false,"maxPending":20},"friendProposal":{"ownerUin":"998877"}},"incidentPilot":{"enabled":true,"graduated":true,"ownerUin":"998877","notifyWarnings":true,"duplicateWindowMinutes":10,"unknownWritesBlockChat":true,"retentionDays":90},"relationshipPilot":{"enabled":true,"graduated":false,"model":"relationship-model","minNewMessages":12,"minBatchMessages":4,"maxEvidenceMessages":24,"maxPeoplePerTurn":3,"maxEvaluationsPerDay":12,"frictionHalfLifeHours":72,"maxMemoryContext":2},"webSearch":{"enabled":true},"dailyMoments":{"enabled":true},"qzoneInteractions":{"enabled":true},"asr":{"enabled":false},"sticker":{"enabled":true,"collectEnabled":true},"multimodalContextPilot":{"enabled":false,"graduated":false},"memory":{"consolidateEnabled":true,"handoffEnabled":true}}"#
            .as_bytes(),
    )
    .unwrap();
    std::fs::write(
        path.join("proactive-state.json"),
        r#"{"lastAttemptAt":1700000007000,"runtimeSnapshot":{"capturedAt":1700000007000,"scheduledWakes":[{"conversationId":"private:100","at":1700000067000,"note":"继续聊","paced":false,"kind":"selfWake"}],"followUpAt":{"private:100":1700000005000}}}"#,
    )
    .unwrap();
    std::fs::write(
        path.join("sessions/session.json"),
        r#"{"id":"session","chatKey":"private:100","startedAt":1700000000000,"endedAt":1700000001000,"status":"done","model":"fixture","persona":"鲸鱼助手","triggerText":"do-not-index-this-transcript","userPrompt":"private transcript must remain in archive only","usage":{"promptTokens":4}}"#
            .as_bytes(),
    )
    .unwrap();
    std::fs::write(
        path.join("daily-moments.json"),
        r#"{"version":1,"records":[{"id":"moment-published","dayKey":"2026-10-01","source":"scheduled","status":"published","publishAttempted":true,"content":"fixture private draft","tid":"qzone-123"},{"id":"moment-unknown","dayKey":"2026-10-02","source":"scheduled","status":"publishing","publishAttempted":true,"content":"possibly published"}],"scheduleSlots":[{"id":"slot-running","dayKey":"2026-10-02","status":"running","recordId":"moment-unknown"}]}"#,
    )
    .unwrap();
    std::fs::write(
        path.join("stickers.json"),
        r#"[{"id":"sticker-1","desc":"测试贴纸","localNote":"fixture note","tags":["测试"],"source":"manual","useCount":3,"localFile":"sticker-assets/test.gif"}]"#,
    )
    .unwrap();
    std::fs::write(
        path.join("qzone-interactions.json"),
        r#"{"version":1,"feedInitializedAt":10,"replyInitializedAt":11,"lastFeedPollAt":12,"lastReplyPollAt":13,"failStreak":0,"feeds":[{"key":"feed-unknown","status":"unknown","post":{"content":"保留的旧动态"}}],"comments":[{"key":"comment-acting","status":"acting","post":{"tid":"post-1"}}],"watchedPosts":[{"key":"123:post-1","uin":"123","tid":"post-1"}],"runs":[{"id":"run-old","status":"idle","actions":[]}] }"#,
    )
    .unwrap();
    let identity = Connection::open(path.join("identity-pilot.sqlite")).unwrap();
    identity.execute_batch(
        "PRAGMA foreign_keys=ON;
         CREATE TABLE people(uin TEXT PRIMARY KEY, primary_name TEXT NOT NULL);
         CREATE TABLE identity_sources(uin TEXT NOT NULL, chat_key TEXT NOT NULL,
           FOREIGN KEY(uin) REFERENCES people(uin));
         CREATE TABLE identity_aliases(uin TEXT NOT NULL, chat_key TEXT NOT NULL, alias TEXT NOT NULL,
           FOREIGN KEY(uin) REFERENCES people(uin));
         CREATE TABLE incoming_friend_requests(id TEXT PRIMARY KEY, uin TEXT NOT NULL);
         CREATE TABLE pilot_assets(id INTEGER PRIMARY KEY, payload BLOB, legacy_text TEXT);
         INSERT INTO people VALUES('100','fixture-person');
         INSERT INTO identity_sources VALUES('100','private:100');
         INSERT INTO identity_aliases VALUES('100','private:100','fixture-alias');
         INSERT INTO incoming_friend_requests VALUES('fr_fixture','100');
         INSERT INTO pilot_assets VALUES(1,x'00ff',CAST(x'ff00' AS TEXT));",
    )
    .unwrap();
    let incident = Connection::open(path.join("incident-pilot.sqlite")).unwrap();
    incident.execute_batch(
        "CREATE TABLE incidents(
            id TEXT PRIMARY KEY,fingerprint TEXT NOT NULL,code TEXT NOT NULL,category TEXT NOT NULL,
            severity TEXT NOT NULL,source TEXT NOT NULL,chat_key TEXT NOT NULL DEFAULT '',
            session_id TEXT NOT NULL DEFAULT '',operation_id TEXT NOT NULL DEFAULT '',state TEXT NOT NULL DEFAULT 'open',
            count INTEGER NOT NULL DEFAULT 1,first_at INTEGER NOT NULL,last_at INTEGER NOT NULL,
            safe_message TEXT NOT NULL,details_json TEXT NOT NULL DEFAULT '{}',stack_hash TEXT NOT NULL DEFAULT '',
            notify_state TEXT NOT NULL DEFAULT 'none',notified_at INTEGER NOT NULL DEFAULT 0,
            notify_error TEXT NOT NULL DEFAULT '',acknowledged_at INTEGER NOT NULL DEFAULT 0,
            resolved_at INTEGER NOT NULL DEFAULT 0,resolution TEXT NOT NULL DEFAULT '',version INTEGER NOT NULL DEFAULT 1
         );
         CREATE TABLE chat_runtime_controls(
            chat_key TEXT PRIMARY KEY,mode TEXT NOT NULL DEFAULT 'auto',reason TEXT NOT NULL DEFAULT '',
            updated_at INTEGER NOT NULL,updated_by TEXT NOT NULL DEFAULT '',version INTEGER NOT NULL DEFAULT 1
         );
         INSERT INTO incidents(id,fingerprint,code,category,severity,source,chat_key,state,count,first_at,last_at,safe_message,details_json,stack_hash,notify_state,version)
           VALUES('inc_fixture','fingerprint-fixture','ERR_FIXTURE','internal','error','test','group:200','open',2,1700000000000,1700000001000,'旧异常条目','{\"safe\":true}','fixture-stack-hash','sent',3);
         INSERT INTO incidents(id,fingerprint,code,category,severity,source,chat_key,state,count,first_at,last_at,safe_message,details_json,notify_state,version)
           VALUES
           ('inc_pending','fp-pending','ERR_PENDING','internal','warning','test','private:100','open',1,1700000002000,1700000002000,'待发送', '{}','pending',1),
           ('inc_failed','fp-failed','ERR_FAILED','internal','error','test','group:200','acknowledged',2,1700000003000,1700000004000,'发送失败', '{}','failed',2),
           ('inc_unknown','fp-unknown','ERR_UNKNOWN','external_write','critical','test','group:200','resolved',1,1700000005000,1700000005000,'结果未知', '{}','unknown',4),
           ('inc_sending','fp-sending','ERR_SENDING','external_write','critical','test','group:200','open',1,1700000006000,1700000006000,'发送中断', '{}','sending',5);
         INSERT INTO chat_runtime_controls(chat_key,mode,reason,updated_at,updated_by,version)
           VALUES('group:200','blocked','管理员维护',1700000000000,'legacy-console',4),
                 ('private:100','continue','单聊恢复',1700000001000,'legacy-console',2);",
    ).unwrap();
    drop(incident);
    let relationship = Connection::open(path.join("relationship-pilot.sqlite")).unwrap();
    relationship.execute_batch(
        "CREATE TABLE relationship_states(
            uin TEXT PRIMARY KEY,familiarity REAL,affinity REAL,friction REAL,friction_updated_at INTEGER,
            message_count INTEGER,active_days INTEGER,direct_interactions INTEGER,chat_count INTEGER,
            last_interaction_at INTEGER,last_evaluated_at INTEGER,evaluator_version TEXT,
            reducer_version TEXT,policy_version TEXT,created_at INTEGER,updated_at INTEGER
         );
         CREATE TABLE relationship_events(
            id TEXT PRIMARY KEY,uin TEXT,event_type TEXT,polarity INTEGER,strength REAL,confidence REAL,
            summary TEXT,evidence_ids TEXT,source_chat_keys TEXT,dedupe_key TEXT UNIQUE,evaluator_version TEXT,
            delta_affinity REAL,delta_friction REAL,created_at INTEGER,applied_at INTEGER
         );
         CREATE TABLE relationship_flags(
            id TEXT PRIMARY KEY,uin TEXT,type TEXT,status TEXT,source_event_id TEXT,
            resolved_by_event_id TEXT,created_at INTEGER,resolved_at INTEGER
         );
         CREATE TABLE relationship_cursors(
            uin TEXT,chat_key TEXT,last_message_id INTEGER,updated_at INTEGER,PRIMARY KEY(uin,chat_key)
         );
         CREATE TABLE relationship_evaluations(
            id TEXT PRIMARY KEY,uin TEXT,status TEXT,evidence_count INTEGER,source_chat_keys TEXT,
            model TEXT,usage_json TEXT,error TEXT,created_at INTEGER,completed_at INTEGER
         );
         INSERT INTO relationship_states VALUES('100',0.7,0.2,0.1,20,42,7,3,2,18,21,'ev1','r1','p1',1,22);
         INSERT INTO relationship_events VALUES('event-1','100','repair',1,0.4,0.9,'修复','[\"m1\"]','[\"group:200\"]','dedupe-1','ev1',0.1,-0.2,20,21);
         INSERT INTO relationship_flags VALUES('flag-1','100','boundary_violation','resolved','event-1','event-2',21,22);
         INSERT INTO relationship_cursors VALUES('100','private:100',1,23);
         INSERT INTO relationship_evaluations VALUES('eval-1','100','done',8,'[\"group:200\"]','fixture-model','{}','',21,22);",
    ).unwrap();
    drop(relationship);
    let database = Connection::open(path.join("messages.sqlite")).unwrap();
    database.execute_batch(
        r#"CREATE TABLE messages(
            chat_key TEXT NOT NULL,id INTEGER NOT NULL,mid TEXT,ts INTEGER NOT NULL,sender_id TEXT,sender_name TEXT,
            text TEXT NOT NULL,self INTEGER NOT NULL DEFAULT 0,reply TEXT,media TEXT NOT NULL DEFAULT '[]',
            mentions_self INTEGER NOT NULL DEFAULT 0,target_user_id TEXT NOT NULL DEFAULT '',event_kind TEXT NOT NULL DEFAULT 'message',
            state TEXT NOT NULL DEFAULT 'pending',lease_id TEXT,attempts INTEGER NOT NULL DEFAULT 0,
            available_at INTEGER NOT NULL DEFAULT 0,error TEXT,PRIMARY KEY(chat_key,id),UNIQUE(chat_key,mid));
         CREATE TABLE outbox(id TEXT PRIMARY KEY,run_id TEXT,chat_key TEXT NOT NULL,state TEXT NOT NULL,payload TEXT NOT NULL,message_id TEXT,error TEXT);
         CREATE TABLE runs(id TEXT PRIMARY KEY,chat_key TEXT NOT NULL,state TEXT NOT NULL,expires_at INTEGER NOT NULL,error TEXT);
         CREATE TABLE recall_notifications(chat_key TEXT NOT NULL,mid TEXT NOT NULL,notified_at INTEGER NOT NULL,PRIMARY KEY(chat_key,mid));
         INSERT INTO messages(chat_key,id,mid,ts,sender_id,sender_name,text,state)
           VALUES('private:100',1,'111',1700000000000,'100','Alice','你好','acked');
         INSERT INTO messages(chat_key,id,mid,ts,sender_id,sender_name,text,self,state)
           VALUES('private:100',2,'112',1700000001000,'self','我','历史回复',1,'acked');
         INSERT INTO messages(chat_key,id,mid,ts,sender_id,sender_name,text,state,lease_id)
           VALUES('group:200',3,'113',1700000002000,'200','Bob','待核对','held','run-held');
         INSERT INTO messages(chat_key,id,mid,ts,sender_id,sender_name,text,state,attempts)
           VALUES('private:100',4,'114',1700000003000,'100','Alice','失败消息','failed',2);
         INSERT INTO messages(chat_key,id,mid,ts,sender_id,sender_name,text,state,available_at)
           VALUES('private:100',5,'115',1700000004000,'100','Alice','延迟消息','pending',9999999999999);
         INSERT INTO outbox(id,run_id,chat_key,state,payload,message_id) VALUES
           ('send-1','run-1','private:100','sent','{"text":"历史回复"}','900'),
           ('send-2','run-2','private:100','unknown','{"text":"不确定"}',NULL),
           ('send-3','run-3','group:200','sending','{"text":"中断"}',NULL),
           ('send-4','run-4','private:100','failed','{"text":"失败"}',NULL);
         INSERT INTO runs(id,chat_key,state,expires_at) VALUES
           ('run-1','private:100','acked',0),('run-held','group:200','held',0);
         INSERT INTO recall_notifications(chat_key,mid,notified_at) VALUES
           ('group:200','113',1700000005000),('private:100','111',1700000006000);"#,
    )
    .unwrap();
}

#[test]
fn importer_refuses_live_writer_assertion_and_builds_private_verified_rust_root() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("node-data");
    let destination = temp.path().join("rinbot-data");
    std::fs::create_dir(&source).unwrap();
    create_node_source(&source);
    let identity_pilot = Connection::open(source.join("identity-pilot.sqlite")).unwrap();
    identity_pilot
        .execute(
            "INSERT INTO identity_aliases(uin,chat_key,alias) VALUES('100','private:100','Alice')",
            [],
        )
        .unwrap();
    drop(identity_pilot);
    std::fs::write(
        source.join("memory/people/_n_Alice.json"),
        r#"{"version":2,"userId":"","name":"Alice","updatedAt":1700000006000,"lastConsolidatedAt":1700000007000,"sourceChatKeys":["private:100"],"impressions":[{"content":"常去露营","createdAt":1700000003000,"sourceChatKeys":["private:100"]}]}"#,
    )
    .unwrap();
    let name_only_backup =
        source.join("memory/backups/consolidation/_n_Alice/1700000004000-name-only.json");
    std::fs::create_dir_all(name_only_backup.parent().unwrap()).unwrap();
    std::fs::write(
        &name_only_backup,
        r#"{"version":1,"reason":"manual-delete","sourceChatKey":"private:100","backedUpAt":1700000004000,"person":{"userId":"","name":"Alice","sourceChatKeys":["private:100"],"impressions":[{"content":"常去露营","sourceChatKeys":["private:100"]}]}}"#,
    )
    .unwrap();
    let preflight = audit_directory(&source);
    assert!(!preflight
        .config
        .as_ref()
        .unwrap()
        .enabled_features
        .is_empty());
    let original_db = std::fs::read(source.join("messages.sqlite")).unwrap();
    let original_config = std::fs::read(source.join("config.json")).unwrap();
    let original_legacy_state = std::fs::read(source.join("memory/private_300.json")).unwrap();
    let original_memory_backup =
        std::fs::read(source.join("memory/backups/consolidation/100/1700000003000-fixture.json"))
            .unwrap();

    assert!(matches!(
        import_stopped_node_directory(&source, &destination, false),
        Err(ImportError::NodeNotStopped)
    ));
    assert!(!destination.exists());

    let report = import_stopped_node_directory(&source, &destination, true).unwrap();
    let consolidation_ledger = rinbot::application::memory_consolidation::MemoryConsolidationStore::open_existing_read_only(
        destination.join("memory-consolidation.sqlite"),
    )
    .unwrap();
    assert_eq!(
        consolidation_ledger.person_updated_at("100").unwrap(),
        1_700_000_006_000
    );
    assert_eq!(
        consolidation_ledger
            .latest_person_cooldown(&["100".into()])
            .unwrap(),
        1_700_000_007_000
    );
    drop(consolidation_ledger);
    assert_eq!(
        preflight.config.as_ref().unwrap().enabled_features,
        report.configuration.enabled_features,
        "read-only preflight and stopped-writer import must report the same effective feature set"
    );
    assert!(report.configuration.enabled_feature_data_missing.is_empty());
    assert!(report.proactive_state.source_present);
    assert!(report.proactive_state.imported);
    assert!(report.proactive_state.verified);
    assert!(report.proactive_state.runtime_snapshot_present);
    assert_eq!(report.proactive_state.source_scheduled_wakes, 1);
    assert_eq!(report.proactive_state.imported_scheduled_wakes, 1);
    assert_eq!(report.proactive_state.verified_scheduled_wakes, 1);
    assert_eq!(report.proactive_state.source_follow_up_cooldowns, 1);
    let proactive = ProactiveStore::open(destination.join("proactive.sqlite"), false)
        .unwrap()
        .expect("imported proactive scheduler state");
    assert_eq!(
        proactive.last_proactive_attempt_at().unwrap(),
        1_700_000_007_000
    );
    let imported_wakes = proactive.list_wakes(10, 0).unwrap();
    assert_eq!(imported_wakes.len(), 1);
    assert_eq!(imported_wakes[0].conversation_id, "private:100");
    assert_eq!(imported_wakes[0].at, 1_700_000_067_000);
    assert_eq!(imported_wakes[0].note, "继续聊");
    assert_eq!(imported_wakes[0].state, "pending");
    assert_eq!(
        proactive.last_follow_up_at("private:100").unwrap(),
        1_700_000_005_000
    );
    assert!(report
        .configuration
        .enabled_features
        .contains(&"webSearch".into()));
    assert!(!report
        .configuration
        .unmapped_enabled_features
        .contains(&"webSearch".into()));
    assert!(report.completed && report.target_integrity_ok);
    assert_eq!(
        report.legacy_archive_sha256,
        fingerprint_legacy_archive(&destination.join("legacy")).unwrap()
    );
    assert_eq!(report.sessions.source_json_files, 1);
    assert_eq!(report.sessions.imported, 1);
    assert_eq!(report.sessions.verified_rows, 1);
    assert_eq!(report.sessions.verified_samples, 1);
    assert_eq!(report.memory_handoffs.source_json_files, 2);
    assert_eq!(report.memory_handoffs.imported, 1);
    assert_eq!(report.memory_handoffs.expired, 1);
    assert_eq!(report.memory_handoffs.legacy_state_files, 2);
    assert_eq!(report.memory_handoffs.converted_legacy_states, 1);
    assert_eq!(report.memory_handoffs.verified_rows, 2);
    assert_eq!(report.memory_handoffs.verified_samples, 2);
    assert_eq!(report.global_person_memory.source_files, 2);
    assert_eq!(report.global_person_memory.source_impressions, 3);
    assert_eq!(report.global_person_memory.resolved_name_only_files, 1);
    assert_eq!(report.global_person_memory.archived_only_files, 0);
    assert_eq!(report.global_person_memory.imported_chat_scoped_rows, 4);
    assert_eq!(report.global_person_memory.target_unique_rows, 4);
    assert_eq!(report.global_person_memory.verified_rows, 4);
    assert_eq!(report.global_person_memory.verified_samples, 3);
    assert_eq!(report.memory_consolidation_backups.source_files, 2);
    assert_eq!(report.memory_consolidation_backups.verified_files, 2);
    assert_eq!(report.memory_consolidation_backups.restorable_files, 2);
    assert_eq!(
        report.memory_consolidation_backups.resolved_name_only_files,
        1
    );
    assert_eq!(
        report
            .memory_consolidation_backups
            .imported_restorable_files,
        2
    );
    assert_eq!(report.memory_consolidation_backups.archived_only_files, 0);
    assert!(
        report
            .memory_consolidation_backups
            .available_to_rust_restore_ui
    );
    let mut identity =
        rinbot::identity::IdentityStore::open(destination.join("identity.sqlite")).unwrap();
    let node_backup_history = identity
        .node_memory_consolidation_backup_history("100", 50)
        .unwrap();
    assert_eq!(node_backup_history.len(), 2);
    let original_backup = node_backup_history
        .iter()
        .find(|backup| backup.reason == "consolidation")
        .unwrap();
    assert_eq!(
        identity
            .consolidation_memory("100", "group:200")
            .unwrap()
            .unwrap()
            .1,
        ["喜欢徒步", "常聊露营"]
    );
    assert_eq!(
        identity
            .consolidation_memory("100", "private:100")
            .unwrap()
            .unwrap()
            .1,
        ["喜欢徒步", "常去露营"]
    );
    let dated = identity
        .consolidation_memory_with_evidence("100", "group:200")
        .unwrap()
        .unwrap()
        .1;
    assert_eq!(dated[0].content, "喜欢徒步");
    assert_eq!(dated[0].created_at, 1_700_000_000_000);
    assert_eq!(dated[0].observed_at, 1_700_000_001_000);
    assert_eq!(dated[0].origin, "model");
    let restored_node_backup = identity
        .restore_node_memory_consolidation_backup(
            "100",
            "group:200",
            &original_backup.backup_key,
            0,
            1_700_000_004_000,
        )
        .unwrap();
    assert_eq!(restored_node_backup.revision, 1);
    assert_eq!(restored_node_backup.impressions, ["Node 整理前快照印象"]);
    drop(identity);
    let identity_db = Connection::open(destination.join("identity.sqlite")).unwrap();
    let memory_timestamps: (i64, i64, String) = identity_db
        .query_row(
            "SELECT created_at,observed_at,origin FROM legacy_memory_refs
             WHERE uin='100' AND chat_key='group:200' AND content='喜欢徒步'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        memory_timestamps,
        (1_700_000_000_000, 1_700_000_001_000, "model".into())
    );
    drop(identity_db);
    let runtime_store = SqliteStore::open(destination.join("rinbot.sqlite")).unwrap();
    let converted = runtime_store
        .conversation_handoff("private:300", 0)
        .unwrap()
        .unwrap();
    assert_eq!(converted.topic, "待办主题；补充主题");
    assert_eq!(converted.summary, "下一步安排");
    assert_eq!(converted.next_step, "下一步安排");
    assert_eq!(converted.source_session_id, "legacy-migration");
    let existing = runtime_store
        .conversation_handoff("group:200", 1_700_000_000_000)
        .unwrap()
        .unwrap();
    assert_eq!(existing.topic, "持续讨论");
    drop(runtime_store);
    assert_eq!(
        std::fs::read(source.join("memory/private_300.json")).unwrap(),
        original_legacy_state,
        "import must never mutate the source legacy memory file"
    );
    assert_eq!(
        std::fs::read(
            destination.join("legacy/memory/backups/consolidation/100/1700000003000-fixture.json")
        )
        .unwrap(),
        original_memory_backup,
        "the byte-exact legacy archive remains the rollback copy even after Rust makes the snapshot restorable"
    );
    assert!(report.daily_moments_archive_present);
    assert!(!report.enabled_daily_moments_data_missing);
    assert_eq!(report.daily_moments.source_records, 2);
    assert_eq!(report.daily_moments.imported_records, 2);
    assert_eq!(report.daily_moments.verified_record_rows, 2);
    assert_eq!(report.daily_moments.source_schedule_slots, 1);
    assert_eq!(report.daily_moments.imported_schedule_slots, 1);
    assert_eq!(report.daily_moments.verified_schedule_rows, 1);
    assert_eq!(report.daily_moments.published_records, 1);
    assert_eq!(report.daily_moments.held_unknown_records, 1);
    assert_eq!(report.daily_moments.verified_record_samples, 2);
    assert_eq!(report.daily_moments.verified_schedule_samples, 1);
    assert!(report.qzone_interactions_archive_present);
    assert!(!report.enabled_qzone_interactions_data_missing);
    assert!(report.sticker_archive_present);
    assert!(!report.enabled_sticker_data_missing);
    assert_eq!(report.stickers.source_entries, 1);
    assert_eq!(report.stickers.imported_entries, 1);
    assert_eq!(report.stickers.verified_rows, 1);
    assert_eq!(report.stickers.verified_samples, 1);
    assert_eq!(report.sticker_assets_copied, 1);
    assert_eq!(
        std::fs::read(destination.join("sticker-assets/test.gif")).unwrap(),
        b"GIF89a-fixture-image"
    );
    let stickers = rinbot::stickers::StickerStore::open(destination.join("stickers.sqlite"), false)
        .unwrap()
        .unwrap();
    let migrated = stickers.list("测试贴纸", 10, 0).unwrap();
    assert_eq!(migrated.len(), 1);
    assert_eq!(migrated[0].local_note, "fixture note");
    assert_eq!(migrated[0].use_count, 3.0);
    drop(stickers);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(destination.join("stickers.sqlite"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert_eq!(report.qzone_interactions.source_items, 4);
    assert_eq!(report.qzone_interactions.imported_items, 4);
    assert_eq!(report.qzone_interactions.verified_item_rows, 4);
    assert_eq!(report.qzone_interactions.source_metadata_entries, 6);
    assert_eq!(report.qzone_interactions.imported_metadata_entries, 6);
    assert_eq!(report.qzone_interactions.verified_metadata_rows, 6);
    assert_eq!(report.qzone_interactions.held_unknown_items, 2);
    assert_eq!(report.qzone_interactions.interrupted_runs, 0);
    assert_eq!(report.qzone_interactions.verified_samples, 3);
    assert_eq!(report.pilot_databases.len(), 3);
    assert_eq!(report.pilot_databases[0].domain, "identity");
    assert_eq!(report.pilot_databases[0].total_rows, 6);
    assert_eq!(report.pilot_databases[0].verified_rows, 6);
    assert_eq!(report.pilot_databases[0].verified_samples, 6);
    assert!(report.configuration.legacy_config_present);
    assert!(report.configuration.legacy_config_valid_json);
    assert!(!report.configuration.secrets_exposed_in_report);
    assert!(report.configuration.core_runtime_configuration_mapped);
    assert!(report.configuration.unmapped_enabled_features.is_empty());
    assert!(report.configuration.enabled_pilot_data_missing.is_empty());
    let qzone = rinbot::qzone_interactions::QzoneInteractionsStore::open_existing_read_only(
        destination.join("qzone-interactions.sqlite"),
    )
    .unwrap();
    assert!(qzone.integrity_ok().unwrap());
    assert_eq!(qzone.counts().unwrap(), 4);
    let moments = rinbot::daily_moments::DailyMomentsStore::open_existing_read_only(
        destination.join("daily-moments.sqlite"),
    )
    .unwrap();
    assert!(moments.integrity_ok().unwrap());
    assert_eq!(moments.counts().unwrap(), (2, 1));
    let unknown_record = moments.record("moment-unknown").unwrap().unwrap();
    assert_eq!(unknown_record["status"], "publish-unknown");
    assert_eq!(unknown_record["publishAttempted"], true);
    assert_eq!(report.incident_records_imported, 5);
    assert_eq!(report.incident_chat_controls_imported, 2);
    let incidents = rinbot::incident::IncidentStore::open(
        destination.join("incident-pilot.sqlite"),
        rinbot::incident::IncidentSettings {
            enabled: false,
            graduated: false,
            notify_warnings: true,
            duplicate_window_minutes: 10,
            retention_days: 90,
            unknown_writes_block_chat: false,
        },
    )
    .unwrap();
    for id in [
        "inc_fixture",
        "inc_pending",
        "inc_failed",
        "inc_unknown",
        "inc_sending",
    ] {
        assert!(
            incidents.get(id).unwrap().is_some(),
            "missing incident {id}"
        );
    }
    let baseline = incidents.get("inc_fixture").unwrap().unwrap();
    assert_eq!(baseline.stack_hash, "fixture-stack-hash");
    assert_eq!(baseline.notify_state, "sent");
    assert_eq!(baseline.count, 2);
    assert_eq!(baseline.first_at, 1_700_000_000_000);
    assert_eq!(baseline.last_at, 1_700_000_001_000);
    assert_eq!(baseline.details["safe"], true);
    assert_eq!(
        incidents.get("inc_pending").unwrap().unwrap().notify_state,
        "pending"
    );
    assert_eq!(
        incidents.get("inc_failed").unwrap().unwrap().notify_state,
        "failed"
    );
    assert_eq!(
        incidents.get("inc_failed").unwrap().unwrap().state,
        "acknowledged"
    );
    assert_eq!(
        incidents.get("inc_unknown").unwrap().unwrap().notify_state,
        "unknown"
    );
    assert_eq!(
        incidents.get("inc_unknown").unwrap().unwrap().state,
        "resolved"
    );
    let interrupted = incidents.get("inc_sending").unwrap().unwrap();
    assert_eq!(interrupted.notify_state, "unknown");
    assert!(interrupted.notify_error.contains("结果未知"));
    let controls = incidents.chat_controls(50).unwrap();
    assert_eq!(controls.len(), 2);
    let blocked = incidents.chat_control("group:200").unwrap();
    assert_eq!(blocked.mode, "blocked");
    assert_eq!(blocked.reason, "管理员维护");
    assert_eq!(blocked.updated_by, "legacy-console");
    assert_eq!(blocked.version, 4);
    assert_eq!(
        incidents.chat_control("private:100").unwrap().mode,
        "continue"
    );
    assert_eq!(report.pilot_databases[2].domain, "relationship");
    assert_eq!(report.pilot_databases[2].total_rows, 5);
    assert_eq!(report.pilot_databases[2].verified_rows, 5);
    assert_eq!(report.pilot_databases[2].verified_samples, 5);
    assert_eq!(report.relationship_records_imported.states, 1);
    assert_eq!(report.relationship_records_imported.events, 1);
    assert_eq!(report.relationship_records_imported.flags, 1);
    assert_eq!(report.relationship_records_imported.cursors, 1);
    assert_eq!(report.relationship_records_imported.evaluations, 1);
    let runtime_store = SqliteStore::open(destination.join("rinbot.sqlite")).unwrap();
    let imported_handoff = runtime_store
        .conversation_handoff("group:200", 4_000_000_000_000)
        .unwrap()
        .unwrap();
    assert_eq!(imported_handoff.topic, "持续讨论");
    assert_eq!(imported_handoff.summary, "下一轮继续验证");
    assert!(runtime_store
        .conversation_handoff("private:100", 4_000_000_000_000)
        .unwrap()
        .is_none());
    let imported_manifest = rinbot::runtime::config::ImportedNodeRuntimeConfig::read(
        &destination.join("rinbot-runtime.json"),
    )
    .unwrap();
    let relationship_settings = imported_manifest.relationship_pilot.unwrap();
    assert!(relationship_settings.enabled);
    assert!(!relationship_settings.graduated);
    assert_eq!(relationship_settings.model, "relationship-model");
    assert_eq!(relationship_settings.min_new_messages, 12);
    assert_eq!(relationship_settings.max_evidence_messages, 24);
    let relationship_store = rinbot::relationship::RelationshipStore::open_existing_read_only(
        destination.join("relationship-pilot.sqlite"),
    )
    .unwrap();
    assert!(relationship_store.integrity_ok().unwrap());
    assert_eq!(relationship_store.counts().unwrap().events, 1);
    assert_eq!(
        relationship_store.cursors("100").unwrap()["private:100"],
        2,
        "per-chat Node cursor must map to the imported global RInBot message id"
    );
    assert_eq!(
        relationship_store.list_states(10, 0).unwrap()[0].message_count,
        42
    );
    let imported_config_path = destination.join("rinbot-runtime.json");
    let imported_config =
        rinbot::runtime::config::ImportedNodeRuntimeConfig::read(&imported_config_path).unwrap();
    assert_eq!(
        imported_config.unmigrated_enabled_features.as_deref(),
        Some(report.configuration.unmapped_enabled_features.as_slice())
    );
    assert_eq!(
        imported_config.enabled_feature_data_missing.as_deref(),
        Some(report.configuration.enabled_feature_data_missing.as_slice())
    );
    assert_eq!(imported_config.model_id.as_deref(), Some("fixture-model"));
    assert_eq!(imported_config.identity_pilot_enabled, Some(true));
    assert_eq!(imported_config.identity_pilot_graduated, Some(true));
    assert!(!report
        .configuration
        .unmapped_enabled_features
        .contains(&"modelFallback".to_owned()));
    assert_eq!(imported_config.incoming_friend_request_enabled, Some(true));
    assert_eq!(imported_config.incoming_friend_auto_whitelist, Some(false));
    assert_eq!(imported_config.incoming_friend_max_pending, Some(20));
    assert_eq!(imported_config.incident_pilot_enabled, Some(true));
    assert_eq!(imported_config.incident_pilot_graduated, Some(true));
    assert_eq!(imported_config.multimodal_context_enabled, Some(false));
    assert_eq!(imported_config.multimodal_context_graduated, Some(false));
    assert_eq!(
        imported_config.incident_unknown_writes_block_chat,
        Some(true)
    );
    assert_eq!(
        imported_config.identity_owner_uin.as_deref(),
        Some("998877")
    );
    assert_eq!(
        imported_config.model_api_key.as_deref(),
        Some("never-print-this")
    );
    let imported_fallback = imported_config.model_fallback.as_ref().unwrap();
    assert!(imported_fallback.enabled);
    assert_eq!(imported_fallback.base_url, "https://backup.invalid/v1");
    assert_eq!(imported_fallback.model, "backup-fixture-model");
    assert_eq!(imported_fallback.timeout_ms, 45_000);
    assert_eq!(
        imported_config.model_fallback_api_key.as_deref(),
        Some("fallback-fixture-secret")
    );
    let imported_vision = imported_config.model_vision.as_ref().unwrap();
    assert!(imported_vision.enabled);
    assert_eq!(imported_vision.base_url, "https://vision.invalid/v1");
    assert_eq!(imported_vision.model, "vision-fixture");
    assert_eq!(imported_vision.timeout_ms, 55_000);
    assert_eq!(
        imported_config.model_vision_api_key.as_deref(),
        Some("vision-fixture-secret")
    );
    assert_eq!(imported_config.allow_groups.as_deref(), Some("200"));
    assert_eq!(imported_config.random_response_percent, Some(15.0));
    assert_eq!(imported_config.unified_tier, Some(false));
    assert_eq!(
        imported_config.blocklist.as_ref().unwrap()["200"],
        ["300", "400"]
    );
    assert_eq!(
        imported_config.group_random_percent.as_ref().unwrap()["200"],
        0.0
    );
    assert_eq!(
        imported_config.group_random_percent.as_ref().unwrap()["201"],
        42.5
    );
    assert_eq!(
        imported_config.trigger_keywords.as_deref(),
        Some("测试,帮助")
    );
    assert_eq!(
        imported_config.system_prompt.as_deref(),
        Some("中文角色\n\n附加规则：\n保持简洁")
    );
    let imported_permissions = std::fs::metadata(imported_config_path)
        .unwrap()
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(imported_permissions.mode() & 0o777, 0o600);
    }
    assert_eq!(report.normalized.messages, 5);
    assert_eq!(report.normalized.outbox, 4);
    assert_eq!(report.normalized.runs, 2);
    assert_eq!(report.normalized.verified_message_rows, 5);
    assert_eq!(report.normalized.verified_outbox_rows, 4);
    assert_eq!(report.normalized.verified_run_rows, 2);
    assert_eq!(report.normalized.verified_recall_notification_rows, 2);
    assert_eq!(report.normalized.message_states.get("acked"), Some(&2));
    assert_eq!(report.normalized.message_states.get("held"), Some(&1));
    assert_eq!(report.normalized.outbox_states.get("sending"), Some(&1));
    assert_eq!(report.normalized.verified_message_samples, 3);
    assert_eq!(report.normalized.verified_outbox_samples, 3);
    assert_eq!(report.normalized.recall_notifications_imported, 2);
    assert_eq!(
        std::fs::read(destination.join("legacy/messages.sqlite")).unwrap(),
        original_db
    );
    assert_eq!(
        std::fs::read(destination.join("legacy/incident-pilot.sqlite")).unwrap(),
        std::fs::read(source.join("incident-pilot.sqlite")).unwrap()
    );
    assert_eq!(
        std::fs::read(destination.join("legacy/config.json")).unwrap(),
        original_config
    );
    assert_eq!(
        std::fs::read(destination.join("legacy/sessions/session.json")).unwrap(),
        std::fs::read(source.join("sessions/session.json")).unwrap()
    );
    let archived_incidents = rinbot::incident::IncidentStore::open(
        destination.join("incident-pilot.sqlite"),
        rinbot::incident::IncidentSettings {
            enabled: false,
            graduated: true,
            notify_warnings: true,
            duplicate_window_minutes: 10,
            retention_days: 90,
            unknown_writes_block_chat: false,
        },
    )
    .unwrap();
    assert!(!archived_incidents.status().unwrap().active);
    assert_eq!(
        archived_incidents
            .get("inc_fixture")
            .unwrap()
            .unwrap()
            .stack_hash,
        "fixture-stack-hash"
    );
    assert_eq!(
        archived_incidents.chat_control("group:200").unwrap().mode,
        "blocked"
    );
    let public_report = std::fs::read_to_string(destination.join("migration-report.json")).unwrap();
    assert!(!public_report.contains("never-print-this"));
    assert!(!public_report.contains("vision-fixture-secret"));
    assert!(!public_report.contains("private transcript must remain"));

    let store = SqliteStore::open(destination.join("rinbot.sqlite")).unwrap();
    assert_eq!(store.counts().unwrap().messages, 5);
    assert_eq!(store.counts().unwrap().unknown_outbox, 2);
    assert_eq!(
        store
            .recall_notification_status("group:200", "113")
            .unwrap()
            .as_deref(),
        Some("sent")
    );
    assert_eq!(
        store
            .recall_notification_status("private:100", "111")
            .unwrap()
            .as_deref(),
        Some("sent")
    );
    assert_eq!(
        report.pilot_databases[0]
            .tables
            .iter()
            .map(|table| table.imported_rows)
            .sum::<u64>(),
        6
    );
    let database = Connection::open(destination.join("rinbot.sqlite")).unwrap();
    let pilot_asset: String = database
        .query_row(
            "SELECT record_json FROM legacy_domain_records
             WHERE domain='identity' AND source_table='pilot_assets'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(pilot_asset.contains("$sqlite_blob_hex"));
    assert!(pilot_asset.contains("$sqlite_text_bytes_hex"));
    let session_summary: String = database
        .query_row(
            "SELECT summary_json FROM legacy_sessions WHERE session_id='session'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(session_summary.contains("鲸鱼助手"));
    assert!(!session_summary.contains("private transcript must remain"));
    assert!(!session_summary.contains("do-not-index-this-transcript"));
    let unknown = store.unknown_outbox().unwrap();
    assert_eq!(unknown.len(), 2);
    let context = store
        .conversation_context("private:100", -1, "bot", 20)
        .unwrap();
    assert!(context
        .iter()
        .any(|entry| entry.is_assistant && entry.text == "历史回复"));
    assert!(!context
        .iter()
        .any(|entry| entry.text == "失败消息" && entry.is_assistant));
    let delayed_id = store
        .queued_inbox_ids(0, 10)
        .unwrap()
        .into_iter()
        .find(|id| store.inbox_available_at(*id).unwrap() == Some(9_999_999_999_999))
        .unwrap();
    assert!(!store.claim_inbox(delayed_id, "early", 30_000).unwrap());
    assert!(matches!(
        import_stopped_node_directory(&source, &destination, true),
        Err(ImportError::UnsafeDestination)
    ));
}

#[test]
fn enabled_memory_consolidation_is_data_blocked_by_unresolved_name_only_archived_backups() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("node-data");
    let destination = temp.path().join("rinbot-data");
    std::fs::create_dir(&source).unwrap();
    create_node_source(&source);
    let name_only = source.join("memory/backups/consolidation/_n_unknown/1700000005000-name.json");
    std::fs::create_dir_all(name_only.parent().unwrap()).unwrap();
    std::fs::write(
        &name_only,
        r#"{"version":1,"reason":"manual-delete","sourceChatKey":"","backedUpAt":1700000005000,"person":{"userId":"","name":"重名用户","impressions":[{"content":"无法确认身份的历史印象"}]}}"#,
    )
    .unwrap();

    let report = import_stopped_node_directory(&source, &destination, true).unwrap();
    assert_eq!(report.memory_consolidation_backups.source_files, 2);
    assert_eq!(
        report
            .memory_consolidation_backups
            .imported_restorable_files,
        1
    );
    assert_eq!(report.memory_consolidation_backups.archived_only_files, 1);
    assert!(
        !report
            .memory_consolidation_backups
            .available_to_rust_restore_ui
    );
    assert!(report
        .configuration
        .enabled_feature_data_missing
        .contains(&"memoryConsolidate".to_owned()));
    let imported_config = rinbot::runtime::config::ImportedNodeRuntimeConfig::read(
        &destination.join(rinbot::runtime::config::IMPORTED_CONFIG_FILE),
    )
    .unwrap();
    assert!(imported_config
        .enabled_feature_data_missing
        .unwrap()
        .contains(&"memoryConsolidate".to_owned()));
    assert!(destination
        .join("legacy/memory/backups/consolidation/_n_unknown/1700000005000-name.json")
        .is_file());
}

#[test]
fn enabled_memory_consolidation_unresolved_name_only_person_files_are_archived_and_blocked() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("node-data");
    let destination = temp.path().join("rinbot-data");
    std::fs::create_dir(&source).unwrap();
    create_node_source(&source);
    let name_only = source.join("memory/people/_n_unknown.json");
    std::fs::write(
        &name_only,
        r#"{"version":2,"userId":"","name":"无法匹配的人","sourceChatKeys":["private:100"],"impressions":[{"content":"身份不确定的旧印象","sourceChatKeys":["private:100"]}]}"#,
    )
    .unwrap();

    let report = import_stopped_node_directory(&source, &destination, true).unwrap();
    assert_eq!(report.global_person_memory.source_files, 2);
    assert_eq!(report.global_person_memory.resolved_name_only_files, 0);
    assert_eq!(report.global_person_memory.archived_only_files, 1);
    assert!(report
        .configuration
        .enabled_feature_data_missing
        .contains(&"memoryConsolidate".to_owned()));
    let imported_config = rinbot::runtime::config::ImportedNodeRuntimeConfig::read(
        &destination.join(rinbot::runtime::config::IMPORTED_CONFIG_FILE),
    )
    .unwrap();
    assert!(imported_config
        .enabled_feature_data_missing
        .unwrap()
        .contains(&"memoryConsolidate".to_owned()));
    assert!(destination
        .join("legacy/memory/people/_n_unknown.json")
        .is_file());
}

#[test]
fn missing_enabled_feature_archive_is_persisted_as_an_active_mode_blocker() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("node-data-missing-enabled-archive");
    let destination = temp.path().join("rinbot-data-missing-enabled-archive");
    std::fs::create_dir(&source).unwrap();
    create_node_source(&source);
    std::fs::remove_file(source.join("qzone-interactions.json")).unwrap();

    let report = import_stopped_node_directory(&source, &destination, true).unwrap();
    assert!(report.enabled_qzone_interactions_data_missing);
    assert!(report
        .configuration
        .enabled_feature_data_missing
        .contains(&"qzoneInteractions".into()));
    let manifest = rinbot::runtime::config::ImportedNodeRuntimeConfig::read(
        &destination.join("rinbot-runtime.json"),
    )
    .unwrap();
    assert!(manifest
        .enabled_feature_data_missing
        .as_ref()
        .unwrap()
        .contains(&"qzoneInteractions".into()));
    let mut cli = rinbot::runtime::config::Cli::try_parse_from(["RInBot"]).unwrap();
    manifest.apply_defaults(&mut cli);
    assert!(cli.feature_data_audit_complete);
    assert!(cli
        .enabled_feature_data_missing
        .contains(&"qzoneInteractions".into()));
    assert!(!source.join("qzone-interactions.json").exists());
}

#[test]
fn missing_enabled_proactive_runtime_snapshot_blocks_active_mode() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("node-data-without-wake-snapshot");
    let destination = temp.path().join("rinbot-data-without-wake-snapshot");
    std::fs::create_dir(&source).unwrap();
    create_node_source(&source);
    std::fs::write(
        source.join("proactive-state.json"),
        r#"{"lastAttemptAt":1700000007000}"#,
    )
    .unwrap();

    let report = import_stopped_node_directory(&source, &destination, true).unwrap();
    assert!(!report.proactive_state.runtime_snapshot_present);
    assert!(report
        .configuration
        .enabled_feature_data_missing
        .contains(&"selfWake".to_owned()));
    assert!(report
        .configuration
        .enabled_feature_data_missing
        .contains(&"followUp".to_owned()));
    let manifest = rinbot::runtime::config::ImportedNodeRuntimeConfig::read(
        &destination.join("rinbot-runtime.json"),
    )
    .unwrap();
    assert!(manifest
        .enabled_feature_data_missing
        .unwrap()
        .contains(&"selfWake".to_owned()));
    assert!(
        !destination.join("proactive.sqlite").exists() || {
            let store = ProactiveStore::open(destination.join("proactive.sqlite"), false)
                .unwrap()
                .unwrap();
            store.list_wakes(10, 0).unwrap().is_empty()
        }
    );
}

#[test]
fn proactive_runtime_snapshot_rejects_unprojected_node_timer_handles() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("node-data-with-invalid-wake-snapshot");
    let destination = temp.path().join("rinbot-data-with-invalid-wake-snapshot");
    std::fs::create_dir(&source).unwrap();
    create_node_source(&source);
    std::fs::write(
        source.join("proactive-state.json"),
        r#"{"lastAttemptAt":1700000007000,"runtimeSnapshot":{"capturedAt":1700000007000,"scheduledWakes":[{"conversationId":"private:100","at":1700000067000,"note":"继续聊","paced":false,"kind":"selfWake","timer":{}}],"followUpAt":{}}}"#,
    )
    .unwrap();

    assert!(import_stopped_node_directory(&source, &destination, true).is_err());
    assert!(!destination.exists());
}

#[test]
fn importer_reports_missing_archives_for_enabled_qzone_features_without_creating_feature_stores() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("node-data");
    let destination = temp.path().join("rinbot-data");
    std::fs::create_dir(&source).unwrap();
    create_node_source(&source);
    std::fs::remove_file(source.join("daily-moments.json")).unwrap();
    std::fs::remove_file(source.join("qzone-interactions.json")).unwrap();
    std::fs::remove_file(source.join("stickers.json")).unwrap();

    let report = import_stopped_node_directory(&source, &destination, true).unwrap();
    assert!(!report.daily_moments_archive_present);
    assert!(report.enabled_daily_moments_data_missing);
    assert_eq!(report.daily_moments.imported_records, 0);
    assert!(!destination.join("daily-moments.sqlite").exists());
    assert!(!report.qzone_interactions_archive_present);
    assert!(report.enabled_qzone_interactions_data_missing);
    assert_eq!(report.qzone_interactions.imported_items, 0);
    assert!(!destination.join("qzone-interactions.sqlite").exists());
    assert!(!report.sticker_archive_present);
    assert!(report.enabled_sticker_data_missing);
    assert_eq!(report.stickers.imported_entries, 0);
    assert!(!destination.join("stickers.sqlite").exists());
}

#[test]
fn importer_refuses_to_publish_a_sticker_library_with_missing_local_media() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("node-data");
    let destination = temp.path().join("rinbot-data");
    std::fs::create_dir(&source).unwrap();
    create_node_source(&source);
    std::fs::remove_file(source.join("sticker-assets/test.gif")).unwrap();

    let audit = audit_directory(&source);
    assert_eq!(audit.sticker_archive.valid_for_import, Some(false));
    assert_eq!(audit.sticker_archive.invalid_assets, 1);
    assert!(audit
        .failures
        .iter()
        .any(|failure| failure.source == "stickers"));
    assert!(matches!(
        import_stopped_node_directory(&source, &destination, true),
        Err(ImportError::InvalidSource)
    ));
    assert!(!destination.exists());
}

#[test]
fn importer_refuses_corrupt_session_json_without_publishing_partial_data_root() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("node-data-corrupt-session");
    let destination = temp.path().join("rinbot-data-corrupt-session");
    std::fs::create_dir(&source).unwrap();
    create_node_source(&source);
    std::fs::write(source.join("sessions/broken.json"), b"{not-json").unwrap();

    let audit = audit_directory(&source);
    assert_eq!(audit.session_archive.source_json_files, 2);
    assert_eq!(audit.session_archive.importable_json_files, 1);
    assert_eq!(audit.session_archive.invalid_json_files, 1);
    assert!(audit
        .failures
        .iter()
        .any(|failure| failure.source == "sessions"));

    let result = import_stopped_node_directory(&source, &destination, true);
    assert!(matches!(result, Err(ImportError::InvalidSource)));
    assert!(!destination.exists());
    assert!(source.join("messages.sqlite").is_file());
    assert_eq!(
        std::fs::read(source.join("sessions/broken.json")).unwrap(),
        b"{not-json"
    );
}

#[test]
fn importer_indexes_node_lone_surrogate_session_text_and_preserves_legacy_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("node-data-surrogate-session");
    let destination = temp.path().join("rinbot-data-surrogate-session");
    std::fs::create_dir(&source).unwrap();
    create_node_source(&source);
    let original =
        br#"{"id":"session","chatKey":"group:200","startedAt":1,"triggerSummary":"\uD800"}"#;
    std::fs::write(source.join("sessions/session.json"), original).unwrap();

    let report = import_stopped_node_directory(&source, &destination, true).unwrap();
    assert_eq!(report.sessions.imported, 1);
    assert_eq!(
        std::fs::read(destination.join("legacy/sessions/session.json")).unwrap(),
        original
    );
    let database = Connection::open(destination.join("rinbot.sqlite")).unwrap();
    let summary: String = database
        .query_row(
            "SELECT summary_json FROM legacy_sessions WHERE session_id='session'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(summary.contains("�"));
}

#[test]
fn importer_preserves_global_vision_disable_over_dedicated_model_flag() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("node-data");
    let destination = temp.path().join("rinbot-data");
    std::fs::create_dir(&source).unwrap();
    create_node_source(&source);
    let config_path = source.join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["api"]["vision"] = serde_json::Value::Bool(false);
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();

    import_stopped_node_directory(&source, &destination, true).unwrap();
    let imported = rinbot::runtime::config::ImportedNodeRuntimeConfig::read(
        &destination.join("rinbot-runtime.json"),
    )
    .unwrap();
    assert!(!imported.model_vision.unwrap().enabled);
}
