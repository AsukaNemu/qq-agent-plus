use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const SERVICE_LABEL: &str = "com.local.rinbot";
const NODE_SERVICE_LABEL: &str = "com.local.qq-agent-plus";
const STOP_BUDGET: Duration = Duration::from_secs(2);
const START_BUDGET: Duration = Duration::from_secs(5);
const READY_BUDGET: Duration = Duration::from_secs(2);
const SWITCH_BUDGET: Duration = Duration::from_secs(10);

#[cfg(all(test, target_os = "macos"))]
#[path = "launchd_drill_tests.rs"]
mod launchd_drill_tests;

pub trait ServiceDriver {
    fn ensure_running(&mut self, label: &str) -> Result<(), String>;
    fn ensure_stopped(&mut self, label: &str) -> Result<(), String>;
    fn stop(&mut self, label: &str, timeout: Duration) -> Result<(), String>;
    fn start(&mut self, label: &str, plist: &Path) -> Result<(), String>;
    fn wait_ready(&mut self, health_url: &str, timeout: Duration) -> Result<(), String>;
    fn wait_node_ready(&mut self, health_url: &str, timeout: Duration) -> Result<(), String>;
    fn verify_node_stopped(&mut self, timeout: Duration) -> Result<(), String>;
    fn verify_rust_stopped(&mut self, timeout: Duration) -> Result<(), String>;
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestartJournal {
    pub format_version: u32,
    pub label: String,
    pub previous_release: PathBuf,
    pub candidate_release: PathBuf,
    pub state: String,
    pub recovery_state: String,
    pub elapsed_ms: u128,
    pub stop_ms: u128,
    pub pointer_switch_ms: u128,
    pub candidate_start_ms: u128,
    pub readiness_ms: u128,
    pub rollback_ms: u128,
    pub failure: String,
    pub recorded_at_ms: u128,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirstCutoverJournal {
    pub format_version: u32,
    pub node_label: String,
    pub rust_label: String,
    pub node_data_dir: PathBuf,
    pub rust_data_dir: PathBuf,
    pub state: String,
    pub recovery_state: String,
    pub elapsed_ms: u128,
    pub failure: String,
    pub recorded_at_ms: u128,
}

#[derive(Default)]
struct PhaseTimings {
    stop_ms: u128,
    pointer_switch_ms: u128,
    candidate_start_ms: u128,
    readiness_ms: u128,
    rollback_ms: u128,
}

#[derive(Debug)]
pub struct RestartError {
    pub message: String,
    pub journal: RestartJournal,
}

#[derive(Debug)]
pub struct FirstCutoverError {
    pub message: String,
    pub journal: FirstCutoverJournal,
}

/// Process-wide advisory lock shared by every service-mutating qqbotctl mode.
/// The lock file intentionally remains after exit; the OS lock itself is
/// released when the process closes the descriptor, including on crashes.
pub struct OperationLock {
    _file: File,
}

impl OperationLock {
    pub fn acquire() -> Result<Self, String> {
        let uid = unsafe { libc::getuid() };
        let path = std::env::temp_dir().join(format!("rinbot-qqbotctl-{uid}.lock"));
        Self::acquire_at(&path, uid)
    }

    fn acquire_at(path: &Path, uid: u32) -> Result<Self, String> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
            let mut options = OpenOptions::new();
            options
                .read(true)
                .write(true)
                .create(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
            let file = options
                .open(path)
                .map_err(|_| "无法安全打开 qqbotctl 操作锁".to_owned())?;
            let metadata = file
                .metadata()
                .map_err(|_| "无法检查 qqbotctl 操作锁".to_owned())?;
            if !metadata.is_file()
                || metadata.uid() != uid
                || metadata.nlink() != 1
                || metadata.mode() & 0o077 != 0
            {
                return Err("qqbotctl 操作锁所有者、类型或权限不安全".into());
            }
            let result = unsafe {
                libc::flock(
                    std::os::fd::AsRawFd::as_raw_fd(&file),
                    libc::LOCK_EX | libc::LOCK_NB,
                )
            };
            if result != 0 {
                return Err("另一个 qqbotctl 服务操作正在执行；未触碰服务".into());
            }
            Ok(Self { _file: file })
        }
        #[cfg(not(unix))]
        {
            let _ = (path, uid);
            Err("qqbotctl 服务操作锁仅支持 Unix/launchd 平台".into())
        }
    }

    #[cfg(test)]
    fn is_held(&self) -> bool {
        self._file.metadata().is_ok()
    }
}

pub fn validate_restart_paths(
    active_link: &Path,
    candidate: &Path,
    release_root: &Path,
) -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let root =
        fs::canonicalize(release_root).map_err(|_| "版本目录不存在或不可规范化".to_owned())?;
    if !root.is_dir() {
        return Err("版本根目录必须是目录".into());
    }
    let link_metadata = fs::symlink_metadata(active_link)
        .map_err(|_| "当前活动版本指针不存在或不可读取".to_owned())?;
    if !link_metadata.file_type().is_symlink() {
        return Err("活动版本指针必须是符号链接".into());
    }
    let old_release =
        fs::canonicalize(active_link).map_err(|_| "当前活动版本指针目标不存在".to_owned())?;
    let candidate =
        fs::canonicalize(candidate).map_err(|_| "候选版本不存在或不可规范化".to_owned())?;
    if !old_release.starts_with(&root) || !candidate.starts_with(&root) {
        return Err("当前版与候选版都必须位于保留的版本根目录内".into());
    }
    if old_release == candidate {
        return Err("候选版本与当前活动版本相同".into());
    }
    if !old_release.is_file() || !candidate.is_file() || !is_executable(&candidate) {
        return Err("当前版和候选版必须存在，候选版必须可执行".into());
    }
    let link_parent = active_link
        .parent()
        .ok_or_else(|| "活动版本指针缺少父目录".to_owned())?;
    if !link_parent.is_dir() {
        return Err("活动版本指针父目录不存在".into());
    }
    Ok((root, old_release, candidate))
}

pub fn validate_first_cutover_candidate(
    active_link: &Path,
    candidate: &Path,
    release_root: &Path,
) -> Result<PathBuf, String> {
    let root =
        fs::canonicalize(release_root).map_err(|_| "保留版本目录不存在或不可规范化".to_owned())?;
    let candidate =
        fs::canonicalize(candidate).map_err(|_| "首次切换候选程序不存在或不可规范化".to_owned())?;
    let active_metadata = fs::symlink_metadata(active_link)
        .map_err(|_| "RInBot 活动版本指针必须预先安装".to_owned())?;
    if !active_metadata.file_type().is_symlink() {
        return Err("RInBot 活动版本指针必须是符号链接".into());
    }
    let active =
        fs::canonicalize(active_link).map_err(|_| "RInBot 活动版本指针目标不可读取".to_owned())?;
    if !candidate.starts_with(&root) || candidate != active {
        return Err("活动版本指针必须预先指向保留版本目录内的候选程序".into());
    }
    if !candidate.is_file() || !is_executable(&candidate) {
        return Err("首次切换候选程序必须是可执行普通文件".into());
    }
    Ok(candidate)
}

pub fn restart_with_rollback(
    driver: &mut impl ServiceDriver,
    active_link: &Path,
    candidate: &Path,
    release_root: &Path,
    plist: &Path,
    health_url: &str,
    journal_path: &Path,
) -> Result<RestartJournal, RestartError> {
    let started = Instant::now();
    let (release_root, old_release, candidate) =
        validate_restart_paths(active_link, candidate, release_root).map_err(|message| {
            RestartError {
                message,
                journal: journal(
                    PathBuf::new(),
                    PathBuf::new(),
                    "blocked",
                    "not-needed",
                    started.elapsed(),
                    "restart paths failed validation".into(),
                ),
            }
        })?;
    if health_url != "http://127.0.0.1:3211/api/status" {
        return Err(RestartError {
            message: "健康检查地址必须是 RInBot 本机 API /api/status".into(),
            journal: journal(
                old_release,
                candidate,
                "blocked",
                "not-needed",
                started.elapsed(),
                "health endpoint failed validation".into(),
            ),
        });
    }
    if let Err(error) = validate_journal_destination(journal_path) {
        return Err(RestartError {
            message: format!("重启日志目标未通过预检：{error}"),
            journal: journal(
                old_release,
                candidate,
                "precondition-failed",
                "not-needed",
                started.elapsed(),
                error,
            ),
        });
    }
    // Reserve a durable recovery marker before asking launchd to change anything.
    // If the process is interrupted between stop/symlink/start, the operator can
    // still distinguish an incomplete operation from a cleanly finished one.
    let progress = journal(
        old_release.clone(),
        candidate.clone(),
        "operation-started",
        "recovery-not-yet-verified",
        started.elapsed(),
        String::new(),
    );
    if let Err(error) = persist_journal(journal_path, &progress) {
        return Err(RestartError {
            message: format!("无法预留重启恢复日志，未触碰服务：{error}"),
            journal: progress,
        });
    }
    let old_release_for_rollback = old_release.clone();
    let candidate_for_journal = candidate.clone();
    let mut timings = PhaseTimings::default();
    if let Err(error) = driver.ensure_running(SERVICE_LABEL) {
        let record = journal(
            old_release,
            candidate,
            "precondition-failed",
            "not-needed",
            started.elapsed(),
            error.clone(),
        );
        replace_journal(journal_path, &record).map_err(|journal_error| RestartError {
            message: format!("服务未运行；切换日志写入失败：{journal_error}"),
            journal: record.clone(),
        })?;
        return Err(RestartError {
            message: format!("受控 Rust 更新要求当前 LaunchAgent 已运行：{error}"),
            journal: record,
        });
    }
    let stop_started = Instant::now();
    if let Err(error) = driver.stop(SERVICE_LABEL, STOP_BUDGET) {
        timings.stop_ms = stop_started.elapsed().as_millis();
        let rollback_started = Instant::now();
        let recovery = driver
            .ensure_running(SERVICE_LABEL)
            .or_else(|_| driver.start(SERVICE_LABEL, plist))
            .and_then(|()| driver.wait_ready(health_url, READY_BUDGET));
        timings.rollback_ms = rollback_started.elapsed().as_millis();
        let mut record = journal(
            old_release,
            candidate,
            "stop-failed",
            if recovery.is_ok() {
                "old-service-restored"
            } else {
                "old-service-recovery-failed"
            },
            started.elapsed(),
            error.clone(),
        );
        apply_timings(&mut record, &timings);
        replace_journal(journal_path, &record).map_err(|journal_error| RestartError {
            message: format!("停止服务失败；切换日志也未能保存：{journal_error}"),
            journal: record.clone(),
        })?;
        return Err(RestartError {
            message: if let Err(recovery_error) = recovery {
                format!("停止当前 Rust 服务失败：{error}；旧版恢复失败：{recovery_error}")
            } else {
                format!("停止当前 Rust 服务失败：{error}；旧版已确认就绪")
            },
            journal: record,
        });
    }
    timings.stop_ms = stop_started.elapsed().as_millis();

    let pointer_started = Instant::now();
    let pointer_result = atomic_replace_link(active_link, &candidate, &release_root);
    timings.pointer_switch_ms = pointer_started.elapsed().as_millis();
    if let Err(error) = pointer_result {
        let rollback_started = Instant::now();
        let recovery = atomic_replace_link(active_link, &old_release_for_rollback, &release_root)
            .and_then(|()| driver.start(SERVICE_LABEL, plist))
            .and_then(|()| driver.wait_ready(health_url, READY_BUDGET));
        timings.rollback_ms = rollback_started.elapsed().as_millis();
        let mut record = journal(
            old_release,
            candidate,
            "pointer-switch-failed",
            if recovery.is_ok() {
                "old-service-restored"
            } else {
                "old-service-recovery-failed"
            },
            started.elapsed(),
            error.clone(),
        );
        apply_timings(&mut record, &timings);
        replace_journal(journal_path, &record).map_err(|journal_error| RestartError {
            message: format!("版本指针切换失败；日志写入失败：{journal_error}"),
            journal: record.clone(),
        })?;
        return Err(RestartError {
            message: if let Err(recovery_error) = recovery {
                format!("版本指针切换失败：{error}；旧版恢复失败：{recovery_error}")
            } else {
                format!("版本指针切换失败：{error}；旧版已恢复")
            },
            journal: record,
        });
    }

    let candidate_start = Instant::now();
    let start_result = driver.start(SERVICE_LABEL, plist);
    timings.candidate_start_ms = candidate_start.elapsed().as_millis();
    let readiness_started = Instant::now();
    let candidate_ready = start_result
        .and_then(|()| driver.wait_ready(health_url, READY_BUDGET))
        .and_then(|()| {
            (started.elapsed() <= SWITCH_BUDGET)
                .then_some(())
                .ok_or_else(|| "新版就绪超过 10 秒受控切换门槛".to_owned())
        });
    timings.readiness_ms = readiness_started.elapsed().as_millis();
    if candidate_ready.is_ok() {
        let mut record = journal(
            old_release,
            candidate_for_journal.clone(),
            "candidate-ready",
            "not-needed",
            started.elapsed(),
            String::new(),
        );
        apply_timings(&mut record, &timings);
        if let Err(error) = replace_journal(journal_path, &record) {
            // Readiness without a durable success record is not enough evidence
            // to leave the new release active. Restore the known-good release.
            let rollback_started = Instant::now();
            let rollback = restore_previous_release(
                driver,
                active_link,
                &old_release_for_rollback,
                &release_root,
                plist,
                health_url,
            );
            timings.rollback_ms = timings
                .rollback_ms
                .saturating_add(rollback_started.elapsed().as_millis());
            let recovery_state = if rollback.is_ok() {
                "old-service-restored"
            } else {
                "old-service-recovery-failed"
            };
            let rollback_detail = rollback
                .err()
                .map(|rollback_error| format!("；旧版恢复失败：{rollback_error}"))
                .unwrap_or_else(|| "；旧版已恢复并就绪".into());
            let mut failure_record = journal(
                old_release_for_rollback,
                candidate_for_journal,
                "success-journal-failed",
                recovery_state,
                started.elapsed(),
                format!("候选就绪记录写入失败：{error}{rollback_detail}"),
            );
            apply_timings(&mut failure_record, &timings);
            let journal_recovery = replace_journal(journal_path, &failure_record);
            return Err(RestartError {
                message: format!(
                    "候选服务曾就绪，但成功记录无法持久化；已尝试回滚{rollback_detail}{}",
                    journal_recovery
                        .err()
                        .map(|journal_error| format!("；恢复日志更新失败：{journal_error}"))
                        .unwrap_or_default()
                ),
                journal: failure_record,
            });
        }
        return Ok(record);
    }

    let candidate_error = candidate_ready.unwrap_err();
    let rollback_started = Instant::now();
    let rollback = restore_previous_release(
        driver,
        active_link,
        &old_release_for_rollback,
        &release_root,
        plist,
        health_url,
    );
    timings.rollback_ms = rollback_started.elapsed().as_millis();
    let recovery_state = if rollback.is_ok() {
        "old-service-restored"
    } else {
        "old-service-recovery-failed"
    };
    let mut record = journal(
        old_release_for_rollback,
        candidate_for_journal,
        "candidate-failed",
        recovery_state,
        started.elapsed(),
        candidate_error.clone(),
    );
    apply_timings(&mut record, &timings);
    replace_journal(journal_path, &record).map_err(|error| RestartError {
        message: format!("候选启动失败且回滚结束；重启日志写入失败：{error}"),
        journal: record.clone(),
    })?;
    Err(RestartError {
        message: if let Err(rollback_error) = rollback {
            format!("候选服务未就绪：{candidate_error}；自动回滚失败：{rollback_error}")
        } else {
            format!("候选服务未就绪：{candidate_error}；旧版本已自动恢复")
        },
        journal: record,
    })
}

/// Perform the first Node-to-Rust service handoff. Both LaunchAgents must be
/// preconfigured against separate data directories. The caller captures the
/// in-memory scheduler projection while Node is alive, stops it, imports the
/// final snapshot, and passes the static gates. This function refuses to start
/// RInBot unless Node is unloaded and its write port is closed. If RInBot
/// fails, Node is restored only after RInBot is confirmed stopped.
pub fn cutover_node_to_rust(
    driver: &mut impl ServiceDriver,
    node_data_dir: &Path,
    rust_data_dir: &Path,
    node_plist: &Path,
    rust_plist: &Path,
    journal_path: &Path,
    validate_final_snapshot: impl FnOnce() -> Result<(), String>,
) -> Result<FirstCutoverJournal, FirstCutoverError> {
    let started = Instant::now();
    let node_data_dir = fs::canonicalize(node_data_dir).map_err(|_| FirstCutoverError {
        message: "Node 数据目录不存在或不可规范化；未触碰服务".into(),
        journal: first_cutover_journal(
            PathBuf::new(),
            PathBuf::new(),
            "blocked",
            "not-needed",
            started.elapsed(),
            "invalid Node data directory".into(),
        ),
    })?;
    let rust_data_dir = fs::canonicalize(rust_data_dir).map_err(|_| FirstCutoverError {
        message: "RInBot 数据目录不存在或不可规范化；未触碰服务".into(),
        journal: first_cutover_journal(
            node_data_dir.clone(),
            PathBuf::new(),
            "blocked",
            "not-needed",
            started.elapsed(),
            "invalid RInBot data directory".into(),
        ),
    })?;
    if node_data_dir.starts_with(&rust_data_dir) || rust_data_dir.starts_with(&node_data_dir) {
        return Err(FirstCutoverError {
            message: "Node 与 RInBot 数据目录重叠；未触碰服务".into(),
            journal: first_cutover_journal(
                node_data_dir,
                rust_data_dir,
                "blocked",
                "not-needed",
                started.elapsed(),
                "overlapping data directories".into(),
            ),
        });
    }
    if let Err(error) = validate_journal_destination(journal_path) {
        return Err(FirstCutoverError {
            message: format!("首次切换日志未通过预检；未触碰服务：{error}"),
            journal: first_cutover_journal(
                node_data_dir,
                rust_data_dir,
                "precondition-failed",
                "not-needed",
                started.elapsed(),
                error,
            ),
        });
    }
    if let Err(error) = driver.ensure_stopped(NODE_SERVICE_LABEL) {
        return Err(FirstCutoverError {
            message: format!("最终快照导入前必须先停止 Node LaunchAgent：{error}"),
            journal: first_cutover_journal(
                node_data_dir,
                rust_data_dir,
                "precondition-failed",
                "not-needed",
                started.elapsed(),
                error,
            ),
        });
    }
    if let Err(error) = driver.ensure_stopped(SERVICE_LABEL) {
        return Err(FirstCutoverError {
            message: format!("最终快照导入期间 RInBot 必须保持未加载：{error}"),
            journal: first_cutover_journal(
                node_data_dir,
                rust_data_dir,
                "precondition-failed",
                "not-needed",
                started.elapsed(),
                error,
            ),
        });
    }
    if let Err(error) = driver.verify_rust_stopped(STOP_BUDGET) {
        return Err(FirstCutoverError {
            message: format!("RInBot 未加载但 3211 端口未确认释放；拒绝执行首次切换：{error}"),
            journal: first_cutover_journal(
                node_data_dir,
                rust_data_dir,
                "precondition-failed",
                "not-needed",
                started.elapsed(),
                error,
            ),
        });
    }
    if let Err(error) = driver.verify_node_stopped(STOP_BUDGET) {
        return Err(FirstCutoverError {
            message: format!("Node 写入端口仍开放或无法确认已停止；未启动 RInBot：{error}"),
            journal: first_cutover_journal(
                node_data_dir,
                rust_data_dir,
                "precondition-failed",
                "not-needed",
                started.elapsed(),
                error,
            ),
        });
    }

    let progress = first_cutover_journal(
        node_data_dir.clone(),
        rust_data_dir.clone(),
        "operation-started",
        "recovery-not-yet-verified",
        started.elapsed(),
        String::new(),
    );
    if let Err(error) = persist_journal(journal_path, &progress) {
        return Err(FirstCutoverError {
            message: format!("无法预留首次切换恢复日志，未触碰服务：{error}"),
            journal: progress,
        });
    }

    if let Err(error) = driver.stop(NODE_SERVICE_LABEL, STOP_BUDGET) {
        let recovery = restore_node(driver, node_plist);
        let record = first_cutover_journal(
            node_data_dir,
            rust_data_dir,
            "node-stop-failed",
            if recovery.is_ok() {
                "node-restored"
            } else {
                "node-recovery-failed"
            },
            started.elapsed(),
            error.clone(),
        );
        let journal_error = replace_journal(journal_path, &record).err();
        return Err(FirstCutoverError {
            message: format!(
                "Node 停止未确认：{error}；{}{}",
                recovery
                    .err()
                    .unwrap_or_else(|| "Node 已确认恢复健康".into()),
                journal_error
                    .map(|value| format!("；恢复日志写入失败：{value}"))
                    .unwrap_or_default()
            ),
            journal: record,
        });
    }

    let node_stopped = driver
        .ensure_stopped(NODE_SERVICE_LABEL)
        .and_then(|()| driver.verify_node_stopped(STOP_BUDGET));
    if let Err(error) = node_stopped {
        let record = first_cutover_journal(
            node_data_dir,
            rust_data_dir,
            "node-stop-not-confirmed",
            "node-state-uncertain",
            started.elapsed(),
            error.clone(),
        );
        let journal_error = replace_journal(journal_path, &record).err();
        return Err(FirstCutoverError {
            message: format!(
                "最终快照后 Node 停止状态复核失败，未启动 RInBot：{error}{}",
                journal_error
                    .map(|value| format!("；操作日志写入失败：{value}"))
                    .unwrap_or_default()
            ),
            journal: record,
        });
    }

    if let Err(error) = validate_final_snapshot() {
        let recovery = restore_node(driver, node_plist);
        let record = first_cutover_journal(
            node_data_dir,
            rust_data_dir,
            "post-stop-snapshot-validation-failed",
            if recovery.is_ok() {
                "node-restored"
            } else {
                "node-recovery-failed"
            },
            started.elapsed(),
            error.clone(),
        );
        let journal_error = replace_journal(journal_path, &record).err();
        return Err(FirstCutoverError {
            message: format!(
                "Node 已停写，但最终快照/迁移门禁复核失败，未启动 RInBot：{error}；{}{}",
                recovery
                    .err()
                    .unwrap_or_else(|| "Node 已恢复并确认健康".into()),
                journal_error
                    .map(|value| format!("；操作日志写入失败：{value}"))
                    .unwrap_or_default()
            ),
            journal: record,
        });
    }

    let rust_start = driver
        .start(SERVICE_LABEL, rust_plist)
        .and_then(|()| driver.wait_ready("http://127.0.0.1:3211/api/status", READY_BUDGET));
    if let Err(error) = rust_start {
        let recovery = restore_node_after_candidate(driver, node_plist);
        let record = first_cutover_journal(
            node_data_dir,
            rust_data_dir,
            "rust-start-failed",
            if recovery.is_ok() {
                "node-restored"
            } else {
                "recovery-failed"
            },
            started.elapsed(),
            error.clone(),
        );
        let journal_error = replace_journal(journal_path, &record).err();
        return Err(FirstCutoverError {
            message: format!(
                "RInBot 未通过 readiness：{error}；{}{}",
                recovery
                    .err()
                    .unwrap_or_else(|| "Node 已恢复并确认健康".into()),
                journal_error
                    .map(|value| format!("；恢复日志写入失败：{value}"))
                    .unwrap_or_default()
            ),
            journal: record,
        });
    }

    let record = first_cutover_journal(
        node_data_dir.clone(),
        rust_data_dir.clone(),
        "rust-ready",
        "not-needed",
        started.elapsed(),
        String::new(),
    );
    if let Err(error) = replace_journal(journal_path, &record) {
        let recovery = restore_node_after_candidate(driver, node_plist);
        let failure_record = first_cutover_journal(
            node_data_dir,
            rust_data_dir,
            "success-journal-failed",
            if recovery.is_ok() {
                "node-restored"
            } else {
                "recovery-failed"
            },
            started.elapsed(),
            error.clone(),
        );
        let journal_error = replace_journal(journal_path, &failure_record).err();
        return Err(FirstCutoverError {
            message: format!(
                "RInBot 曾就绪，但成功日志无法持久化；{}{}{}",
                recovery
                    .err()
                    .unwrap_or_else(|| "Node 已恢复并确认健康".into()),
                format!("；原始日志错误：{error}"),
                journal_error
                    .map(|value| format!("；恢复日志更新失败：{value}"))
                    .unwrap_or_default()
            ),
            journal: failure_record,
        });
    }
    Ok(record)
}

fn restore_node_after_candidate(
    driver: &mut impl ServiceDriver,
    node_plist: &Path,
) -> Result<(), String> {
    driver
        .stop(SERVICE_LABEL, STOP_BUDGET)
        .and_then(|()| driver.verify_rust_stopped(STOP_BUDGET))
        .map_err(|error| {
            format!("RInBot LaunchAgent 或 3211 端口停止状态未确认；为避免双写不启动 Node：{error}")
        })?;
    restore_node(driver, node_plist)
}

fn restore_node(driver: &mut impl ServiceDriver, node_plist: &Path) -> Result<(), String> {
    driver
        .ensure_running(NODE_SERVICE_LABEL)
        .or_else(|_| driver.start(NODE_SERVICE_LABEL, node_plist))
        .and_then(|()| driver.wait_node_ready("http://127.0.0.1:3210/healthz", READY_BUDGET))
}

fn first_cutover_journal(
    node_data_dir: PathBuf,
    rust_data_dir: PathBuf,
    state: &str,
    recovery_state: &str,
    elapsed: Duration,
    failure: String,
) -> FirstCutoverJournal {
    FirstCutoverJournal {
        format_version: 1,
        node_label: NODE_SERVICE_LABEL.into(),
        rust_label: SERVICE_LABEL.into(),
        node_data_dir,
        rust_data_dir,
        state: state.into(),
        recovery_state: recovery_state.into(),
        elapsed_ms: elapsed.as_millis(),
        failure,
        recorded_at_ms: now_ms(),
    }
}

fn restore_previous_release(
    driver: &mut impl ServiceDriver,
    active_link: &Path,
    previous_release: &Path,
    release_root: &Path,
    plist: &Path,
    health_url: &str,
) -> Result<(), String> {
    let stop = driver.stop(SERVICE_LABEL, STOP_BUDGET);
    // Restore the persistent pointer even when launchd could not confirm the
    // candidate stopped. Never bootstrap the old release while the candidate
    // may still be alive; that could create two writers to the same data.
    let pointer = atomic_replace_link(active_link, previous_release, release_root);
    match (stop, pointer) {
        (Ok(()), Ok(())) => driver
            .start(SERVICE_LABEL, plist)
            .and_then(|()| driver.wait_ready(health_url, READY_BUDGET)),
        (Err(stop_error), Ok(())) => Err(format!(
            "候选服务停止未确认：{stop_error}；活动指针已恢复旧版，但为避免双实例未启动旧版"
        )),
        (Ok(()), Err(pointer_error)) => Err(format!(
            "候选服务已停止，但旧版指针恢复失败：{pointer_error}；未启动服务"
        )),
        (Err(stop_error), Err(pointer_error)) => Err(format!(
            "候选服务停止未确认：{stop_error}；旧版指针恢复也失败：{pointer_error}"
        )),
    }
}

pub struct LaunchctlDriver {
    domain: String,
    api_token: Option<String>,
}

impl LaunchctlDriver {
    pub fn new() -> Result<Self, String> {
        let output = std::process::Command::new("id")
            .arg("-u")
            .output()
            .map_err(|_| "无法读取当前用户 UID".to_owned())?;
        if !output.status.success() {
            return Err("无法读取当前用户 UID".into());
        }
        let uid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if uid.is_empty() || !uid.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("当前用户 UID 格式无效".into());
        }
        if uid == "0" {
            return Err("拒绝以 root 身份控制用户 LaunchAgent".into());
        }
        Ok(Self {
            domain: format!("gui/{uid}"),
            api_token: None,
        })
    }

    pub fn for_rust_plist(path: &Path) -> Result<Self, String> {
        let mut driver = Self::new()?;
        driver.api_token = rust_api_token_from_plist(path)?;
        Ok(driver)
    }

    fn launchctl(&self, args: &[&str]) -> Result<(), String> {
        self.launchctl_with_timeout(args, START_BUDGET)
    }

    fn launchctl_with_timeout(&self, args: &[&str], timeout: Duration) -> Result<(), String> {
        let status = run_status("launchctl", args, timeout)
            .map_err(|_| "无法在时限内执行 launchctl".to_owned())?;
        if status.success() {
            Ok(())
        } else {
            let code = status.code().unwrap_or(-1);
            Err(format!("launchctl 操作失败（退出码 {code}）"))
        }
    }
}

impl ServiceDriver for LaunchctlDriver {
    fn ensure_running(&mut self, label: &str) -> Result<(), String> {
        let target = format!("{}/{}", self.domain, label);
        let status = run_status("launchctl", &["print", target.as_str()], STOP_BUDGET)
            .map_err(|_| "无法查询当前 RInBot LaunchAgent".to_owned())?;
        if status.success() {
            Ok(())
        } else {
            Err("当前 RInBot LaunchAgent 未加载".into())
        }
    }

    fn ensure_stopped(&mut self, label: &str) -> Result<(), String> {
        let target = format!("{}/{}", self.domain, label);
        let status = run_status("launchctl", &["print", target.as_str()], STOP_BUDGET)
            .map_err(|_| "无法确认 RInBot LaunchAgent 未加载".to_owned())?;
        match launchctl_target_loaded(status)? {
            true => Err("RInBot LaunchAgent 已加载".into()),
            false => Ok(()),
        }
    }

    fn stop(&mut self, label: &str, timeout: Duration) -> Result<(), String> {
        let started = Instant::now();
        let target = format!("{}/{}", self.domain, label);
        let remaining = || timeout.saturating_sub(started.elapsed());
        let status = run_status("launchctl", &["print", target.as_str()], remaining())
            .map_err(|_| "无法查询 LaunchAgent 状态".to_owned())?;
        match launchctl_target_loaded(status)? {
            false => return Ok(()),
            true => {}
        }
        self.launchctl_with_timeout(&["bootout", target.as_str()], remaining())?;
        while started.elapsed() < timeout {
            let budget = remaining();
            if budget.is_zero() {
                break;
            }
            let status = run_status("launchctl", &["print", target.as_str()], budget)
                .map_err(|_| "无法确认 LaunchAgent 已停止".to_owned())?;
            match launchctl_target_loaded(status)? {
                false => return Ok(()),
                true => {}
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err("LaunchAgent 在停止时限内仍处于加载状态".into())
    }

    fn start(&mut self, label: &str, plist: &Path) -> Result<(), String> {
        let started = Instant::now();
        let domain = self.domain.clone();
        let plist = plist
            .to_str()
            .ok_or_else(|| "LaunchAgent plist 路径不是有效 UTF-8".to_owned())?;
        self.launchctl(&["bootstrap", domain.as_str(), plist])?;
        let target = format!("{domain}/{label}");
        let remaining = START_BUDGET.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err("LaunchAgent 启动操作超过 5 秒时限".into());
        }
        let status = run_status("launchctl", &["kickstart", target.as_str()], remaining)
            .map_err(|_| "LaunchAgent kickstart 超过 5 秒启动时限".to_owned())?;
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "LaunchAgent kickstart 失败（退出码 {}）",
                status.code().unwrap_or(-1)
            ))
        }
    }

    fn wait_ready(&mut self, health_url: &str, timeout: Duration) -> Result<(), String> {
        let url = reqwest::Url::parse(health_url).map_err(|_| "RInBot readiness 地址无效")?;
        if url.scheme() != "http"
            || url.host_str() != Some("127.0.0.1")
            || url.path() != "/api/status"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("RInBot readiness 仅允许本机 HTTP /api/status".into());
        }
        let deadline = Instant::now() + timeout;
        let mut last_error = "尚未收到 API readiness 响应".to_owned();
        let auth_header = self
            .api_token
            .as_ref()
            .map(|token| format!("Authorization: Bearer {token}\n"));
        while Instant::now() < deadline {
            let mut args = vec![
                "--fail",
                "--silent",
                "--show-error",
                "--max-time",
                "1",
                "--max-filesize",
                "65536",
            ];
            if auth_header.is_some() {
                args.extend(["--header", "@-"]);
            }
            args.push(health_url);
            let output = run_capture_with_input(
                "curl",
                &args,
                Duration::from_secs(1),
                auth_header.as_deref().map(str::as_bytes),
            );
            if let Ok(output) = output {
                if output.status.success() {
                    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout) {
                        let ready = value.get("mode").and_then(|value| value.as_str())
                            == Some("active")
                            && value
                                .get("onebot_connected")
                                .and_then(|value| value.as_bool())
                                == Some(true)
                            && value.get("core_ready").and_then(|value| value.as_bool())
                                == Some(true);
                        if ready {
                            return Ok(());
                        }
                        last_error =
                            "API 已响应，但 active/OneBot/core readiness 未全部就绪".into();
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(format!("readiness 超时：{last_error}"))
    }

    fn wait_node_ready(&mut self, health_url: &str, timeout: Duration) -> Result<(), String> {
        if health_url != "http://127.0.0.1:3210/healthz" {
            return Err("Node 健康检查地址必须是本机 /healthz".into());
        }
        let deadline = Instant::now() + timeout;
        let mut last_error = "尚未收到 Node health 响应".to_owned();
        while Instant::now() < deadline {
            if let Ok(output) = run_capture(
                "curl",
                &[
                    "--fail",
                    "--silent",
                    "--show-error",
                    "--max-time",
                    "1",
                    "--max-filesize",
                    "65536",
                    health_url,
                ],
                Duration::from_secs(1),
            ) {
                if output.status.success() {
                    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout) {
                        if value.get("ok").and_then(|value| value.as_bool()) == Some(true)
                            && value
                                .get("onebotConnected")
                                .and_then(|value| value.as_bool())
                                == Some(true)
                        {
                            return Ok(());
                        }
                        last_error = "Node 已响应，但服务或 OneBot 未健康".into();
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(format!("Node 健康检查超时：{last_error}"))
    }

    fn verify_node_stopped(&mut self, timeout: Duration) -> Result<(), String> {
        verify_loopback_port_closed(3210, "Node", timeout)
    }

    fn verify_rust_stopped(&mut self, timeout: Duration) -> Result<(), String> {
        verify_loopback_port_closed(3211, "RInBot", timeout)
    }
}

pub(super) fn rust_api_token_from_plist(path: &Path) -> Result<Option<String>, String> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "RInBot plist 不可读")?;
    if !metadata.file_type().is_file() || metadata.len() > 1024 * 1024 {
        return Err("RInBot plist 必须是大小受限的普通文件".into());
    }
    let plist = plist::Value::from_file(path).map_err(|_| "RInBot plist 无法解析")?;
    let dictionary = plist.as_dictionary().ok_or("RInBot plist 缺少字典")?;
    let args = dictionary
        .get("ProgramArguments")
        .and_then(plist::Value::as_array)
        .ok_or("RInBot plist 缺少启动参数")?;
    let mut configured = None;
    for (index, argument) in args.iter().enumerate() {
        let argument = argument.as_string().ok_or("RInBot 启动参数必须为字符串")?;
        let value = if argument == "--api-token" {
            Some(
                args.get(index + 1)
                    .and_then(plist::Value::as_string)
                    .filter(|value| !value.starts_with("--"))
                    .ok_or("RInBot --api-token 缺少值")?,
            )
        } else {
            argument.strip_prefix("--api-token=")
        };
        if let Some(value) = value {
            if configured.replace(value).is_some() {
                return Err("RInBot --api-token 不得重复配置".into());
            }
        }
    }
    let token = configured
        .or_else(|| {
            dictionary
                .get("EnvironmentVariables")
                .and_then(plist::Value::as_dictionary)
                .and_then(|environment| environment.get("QQBOTD_API_TOKEN"))
                .and_then(plist::Value::as_string)
        })
        .unwrap_or("");
    if token.len() > 8_192 || token.bytes().any(|byte| byte.is_ascii_control()) {
        return Err("RInBot API 令牌长度或字符无效".into());
    }
    Ok((!token.is_empty()).then(|| token.to_owned()))
}

fn verify_loopback_port_closed(port: u16, service: &str, timeout: Duration) -> Result<(), String> {
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let started = Instant::now();
    loop {
        let remaining = timeout.saturating_sub(started.elapsed());
        let probe_budget = remaining.min(Duration::from_millis(100));
        if probe_budget.is_zero() {
            return Err(format!("无法在时限内确认 {service} {port} 端口已释放"));
        }
        match std::net::TcpStream::connect_timeout(&address, probe_budget) {
            Ok(_) => return Err(format!("仍有进程监听 {service} 端口 127.0.0.1:{port}")),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => return Err(format!("无法确认 {service} {port} 端口状态")),
        }
        std::thread::sleep(Duration::from_millis(25).min(remaining));
    }
}

fn launchctl_target_loaded(status: std::process::ExitStatus) -> Result<bool, String> {
    if status.success() {
        return Ok(true);
    }
    // launchctl print reports a missing service with bootstrap error 113.
    // Treat every other failure as an inability to prove the single-writer
    // precondition, rather than assuming the candidate is unloaded.
    match status.code() {
        Some(113) => Ok(false),
        Some(code) => Err(format!("launchctl 状态查询失败（退出码 {code}）")),
        None => Err("launchctl 状态查询未正常退出".into()),
    }
}

fn run_status(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> std::io::Result<std::process::ExitStatus> {
    use std::process::Stdio;
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "subprocess timeout",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn run_capture(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> std::io::Result<std::process::Output> {
    run_capture_with_input(program, args, timeout, None)
}

fn run_capture_with_input(
    program: &str,
    args: &[&str],
    timeout: Duration,
    input: Option<&[u8]>,
) -> std::io::Result<std::process::Output> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(input) = input {
        let result = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("missing child input"))?
            .write_all(input);
        if let Err(error) = result {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    }
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output();
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "subprocess timeout",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn atomic_replace_link(link: &Path, target: &Path, root: &Path) -> Result<(), String> {
    let canonical_target = fs::canonicalize(target).map_err(|_| "版本目标不可规范化".to_owned())?;
    if !canonical_target.starts_with(root) {
        return Err("拒绝将版本指针指向保留目录之外".into());
    }
    let parent = link
        .parent()
        .ok_or_else(|| "活动版本指针缺少父目录".to_owned())?;
    let temp_link = parent.join(format!(
        ".rinbot-active-{}-{}.tmp",
        std::process::id(),
        now_ms()
    ));
    #[cfg(unix)]
    std::os::unix::fs::symlink(&canonical_target, &temp_link)
        .map_err(|_| "无法创建临时版本指针".to_owned())?;
    #[cfg(not(unix))]
    return Err("原子版本指针仅支持 Unix/launchd 平台".into());

    if let Err(error) = fs::rename(&temp_link, link) {
        let _ = fs::remove_file(&temp_link);
        return Err(format!("原子替换版本指针失败：{error}"));
    }
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "版本指针已替换，但父目录同步失败".to_owned())
}

fn persist_journal<T: Serialize>(path: &Path, journal: &T) -> Result<(), String> {
    validate_journal_destination(path)?;
    let parent = path.parent().expect("journal validator requires parent");
    let temp = parent.join(format!(
        ".restart-journal-{}-{}.tmp",
        std::process::id(),
        now_ms()
    ));
    let bytes = serde_json::to_vec_pretty(journal).map_err(|_| "序列化重启日志失败".to_owned())?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp)
        .map_err(|_| "创建重启日志临时文件失败".to_owned())?;
    use std::io::Write;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temp);
        return Err(format!("写入重启日志临时文件失败：{error}"));
    }
    if let Err(error) = fs::hard_link(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(format!("保存重启日志失败：{error}"));
    }
    fs::remove_file(&temp).map_err(|_| "重启日志已写入，但临时文件清理失败".to_owned())?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "重启日志已写入，但父目录同步失败".to_owned())
}

fn replace_journal<T: Serialize>(path: &Path, journal: &T) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "预留的重启日志不存在或不可读取".to_owned())?;
    if !metadata.file_type().is_file() {
        return Err("拒绝替换非普通文件或符号链接形式的重启日志".into());
    }
    let parent = path
        .parent()
        .ok_or_else(|| "重启日志缺少父目录".to_owned())?;
    let temp = parent.join(format!(
        ".restart-journal-{}-{}.tmp",
        std::process::id(),
        now_ms()
    ));
    let bytes = serde_json::to_vec_pretty(journal).map_err(|_| "序列化重启日志失败".to_owned())?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp)
        .map_err(|_| "创建重启日志替换文件失败".to_owned())?;
    use std::io::Write;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temp);
        return Err(format!("写入重启日志替换文件失败：{error}"));
    }
    let current = fs::symlink_metadata(path).map_err(|_| "重启日志在更新期间消失".to_owned())?;
    if !current.file_type().is_file() {
        let _ = fs::remove_file(&temp);
        return Err("重启日志在更新期间被替换为非普通文件".into());
    }
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(format!("原子更新重启日志失败：{error}"));
    }
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "重启日志已更新，但父目录同步失败".to_owned())
}

fn validate_journal_destination(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("重启日志路径必须是绝对路径".into());
    }
    let parent = path
        .parent()
        .ok_or_else(|| "重启日志必须位于显式目录".to_owned())?;
    let parent =
        fs::canonicalize(parent).map_err(|_| "重启日志父目录不存在或不可规范化".to_owned())?;
    match fs::symlink_metadata(path) {
        Ok(_) => return Err("重启日志目标已存在；请为每次操作指定新文件".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("重启日志目标不可检查".into()),
    }
    let probe = parent.join(format!(
        ".restart-probe-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map_err(|_| "重启日志父目录不可写".to_owned())?;
    drop(file);
    fs::remove_file(probe).map_err(|_| "无法清理重启日志目录探测文件".to_owned())?;
    Ok(())
}

fn journal(
    previous_release: PathBuf,
    candidate_release: PathBuf,
    state: &str,
    recovery_state: &str,
    elapsed: Duration,
    failure: String,
) -> RestartJournal {
    RestartJournal {
        format_version: 1,
        label: SERVICE_LABEL.into(),
        previous_release,
        candidate_release,
        state: state.into(),
        recovery_state: recovery_state.into(),
        elapsed_ms: elapsed.as_millis(),
        stop_ms: 0,
        pointer_switch_ms: 0,
        candidate_start_ms: 0,
        readiness_ms: 0,
        rollback_ms: 0,
        failure,
        recorded_at_ms: now_ms(),
    }
}

fn apply_timings(journal: &mut RestartJournal, timings: &PhaseTimings) {
    journal.stop_ms = timings.stop_ms;
    journal.pointer_switch_ms = timings.pointer_switch_ms;
    journal.candidate_start_ms = timings.candidate_start_ms;
    journal.readiness_ms = timings.readiness_ms;
    journal.rollback_ms = timings.rollback_ms;
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, fs, rc::Rc};
    use tempfile::tempdir;

    #[derive(Clone)]
    struct FakeDriver {
        events: Rc<RefCell<Vec<String>>>,
        running: bool,
        candidate_ready: bool,
        old_ready: bool,
        journal_path: Option<PathBuf>,
        sabotage_journal_on_start: bool,
        fail_candidate_stop: bool,
        fail_node_stopped_check_after_stop: bool,
        fail_rust_port_check_after_stop: bool,
        stop_calls: u8,
    }

    impl ServiceDriver for FakeDriver {
        fn ensure_running(&mut self, label: &str) -> Result<(), String> {
            self.events.borrow_mut().push(format!("check:{label}"));
            self.running
                .then_some(())
                .ok_or_else(|| "simulated agent not loaded".into())
        }

        fn ensure_stopped(&mut self, label: &str) -> Result<(), String> {
            self.events
                .borrow_mut()
                .push(format!("check-stopped:{label}"));
            let events = self.events.borrow();
            let started = events
                .iter()
                .rposition(|event| event == &format!("start:{label}"));
            let stopped = events
                .iter()
                .rposition(|event| event == &format!("stop:{label}"));
            if started.is_some() && started > stopped {
                Err("simulated agent loaded".into())
            } else {
                Ok(())
            }
        }

        fn stop(&mut self, label: &str, _: Duration) -> Result<(), String> {
            self.events.borrow_mut().push(format!("stop:{label}"));
            self.stop_calls = self.stop_calls.saturating_add(1);
            if self.fail_candidate_stop && self.stop_calls >= 2 {
                return Err("simulated candidate stop timeout".into());
            }
            self.running = false;
            Ok(())
        }

        fn start(&mut self, label: &str, _: &Path) -> Result<(), String> {
            self.events.borrow_mut().push(format!("start:{label}"));
            if self.sabotage_journal_on_start {
                if let Some(path) = &self.journal_path {
                    fs::remove_file(path).unwrap();
                    fs::create_dir(path).unwrap();
                }
                self.sabotage_journal_on_start = false;
            }
            self.running = true;
            Ok(())
        }

        fn wait_ready(&mut self, _: &str, _: Duration) -> Result<(), String> {
            let starts = self
                .events
                .borrow()
                .iter()
                .filter(|event| event.starts_with("start:"))
                .count();
            self.events.borrow_mut().push("ready-check".into());
            let ready = if starts <= 1 {
                self.candidate_ready
            } else {
                self.old_ready
            };
            ready
                .then_some(())
                .ok_or_else(|| "simulated not ready".into())
        }

        fn wait_node_ready(&mut self, _: &str, _: Duration) -> Result<(), String> {
            self.events.borrow_mut().push("node-ready-check".into());
            Ok(())
        }

        fn verify_node_stopped(&mut self, _: Duration) -> Result<(), String> {
            self.events.borrow_mut().push("node-stopped-check".into());
            let stopped = self
                .events
                .borrow()
                .iter()
                .any(|event| event == "stop:com.local.qq-agent-plus");
            if stopped && self.fail_node_stopped_check_after_stop {
                Err("simulated Node listener remains open".into())
            } else {
                Ok(())
            }
        }

        fn verify_rust_stopped(&mut self, _: Duration) -> Result<(), String> {
            self.events
                .borrow_mut()
                .push("rust-port-closed-check".into());
            let stopped = self
                .events
                .borrow()
                .iter()
                .any(|event| event == "stop:com.local.rinbot");
            if stopped && self.fail_rust_port_check_after_stop {
                Err("simulated RInBot listener remains open".into())
            } else {
                Ok(())
            }
        }
    }

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, PathBuf) {
        let temp = tempdir().unwrap();
        let root = temp.path().join("releases");
        fs::create_dir_all(root.join("v1")).unwrap();
        fs::create_dir_all(root.join("v2")).unwrap();
        let old = root.join("v1/RInBot");
        let candidate = root.join("v2/RInBot");
        fs::write(&old, b"old").unwrap();
        fs::write(&candidate, b"new").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::{symlink, PermissionsExt};
            fs::set_permissions(&old, fs::Permissions::from_mode(0o755)).unwrap();
            fs::set_permissions(&candidate, fs::Permissions::from_mode(0o755)).unwrap();
            symlink(&old, temp.path().join("active")).unwrap();
        }
        let plist = temp.path().join("rinbot.plist");
        fs::write(&plist, b"test plist").unwrap();
        let journal = temp.path().join("ops/restart.json");
        fs::create_dir(journal.parent().unwrap()).unwrap();
        (temp, root, candidate, plist, journal)
    }

    #[test]
    fn candidate_ready_switches_pointer_and_persists_journal() {
        let (temp, root, candidate, plist, journal_path) = fixture();
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut driver = FakeDriver {
            events: events.clone(),
            running: true,
            candidate_ready: true,
            old_ready: true,
            journal_path: None,
            sabotage_journal_on_start: false,
            fail_candidate_stop: false,
            fail_node_stopped_check_after_stop: false,
            fail_rust_port_check_after_stop: false,
            stop_calls: 0,
        };
        let result = restart_with_rollback(
            &mut driver,
            &temp.path().join("active"),
            &candidate,
            &root,
            &plist,
            "http://127.0.0.1:3211/api/status",
            &journal_path,
        )
        .unwrap();
        assert_eq!(result.state, "candidate-ready");
        assert_eq!(
            fs::canonicalize(temp.path().join("active")).unwrap(),
            fs::canonicalize(&candidate).unwrap()
        );
        assert_eq!(
            events.borrow().as_slice(),
            [
                "check:com.local.rinbot",
                "stop:com.local.rinbot",
                "start:com.local.rinbot",
                "ready-check"
            ]
        );
        assert_eq!(
            serde_json::from_slice::<RestartJournal>(&fs::read(journal_path).unwrap())
                .unwrap()
                .state,
            "candidate-ready"
        );
    }

    #[test]
    fn first_cutover_stops_node_before_starting_rust_and_records_success() {
        let (temp, _, _, rust_plist, journal_path) = fixture();
        let node_data = temp.path().join("node-data");
        let rust_data = temp.path().join("rust-data");
        fs::create_dir(&node_data).unwrap();
        fs::create_dir(&rust_data).unwrap();
        let node_plist = temp.path().join("node.plist");
        fs::write(&node_plist, b"test node plist").unwrap();
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut driver = FakeDriver {
            events: events.clone(),
            running: true,
            candidate_ready: true,
            old_ready: true,
            journal_path: None,
            sabotage_journal_on_start: false,
            fail_candidate_stop: false,
            fail_node_stopped_check_after_stop: false,
            fail_rust_port_check_after_stop: false,
            stop_calls: 0,
        };

        let record = cutover_node_to_rust(
            &mut driver,
            &node_data,
            &rust_data,
            &node_plist,
            &rust_plist,
            &journal_path,
            || Ok(()),
        )
        .unwrap();

        assert_eq!(record.state, "rust-ready");
        assert_eq!(record.recovery_state, "not-needed");
        let events = events.borrow();
        let node_stopped_check = events
            .iter()
            .position(|event| event == "node-stopped-check")
            .unwrap();
        let node_stop = events
            .iter()
            .position(|event| event == "stop:com.local.qq-agent-plus")
            .unwrap();
        let rust_start = events
            .iter()
            .position(|event| event == "start:com.local.rinbot")
            .unwrap();
        assert!(node_stopped_check < rust_start);
        assert!(node_stop < rust_start, "Node must stop before Rust starts");
        assert_eq!(
            serde_json::from_slice::<FirstCutoverJournal>(&fs::read(journal_path).unwrap())
                .unwrap()
                .state,
            "rust-ready"
        );
    }

    #[test]
    fn first_cutover_revalidates_final_snapshot_before_starting_rust() {
        let (temp, _, _, rust_plist, journal_path) = fixture();
        let node_data = temp.path().join("node-data");
        let rust_data = temp.path().join("rust-data");
        fs::create_dir(&node_data).unwrap();
        fs::create_dir(&rust_data).unwrap();
        let node_plist = temp.path().join("node.plist");
        fs::write(&node_plist, b"test node plist").unwrap();
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut driver = FakeDriver {
            events: events.clone(),
            running: true,
            candidate_ready: true,
            old_ready: true,
            journal_path: None,
            sabotage_journal_on_start: false,
            fail_candidate_stop: false,
            fail_node_stopped_check_after_stop: false,
            fail_rust_port_check_after_stop: false,
            stop_calls: 0,
        };

        let error = cutover_node_to_rust(
            &mut driver,
            &node_data,
            &rust_data,
            &node_plist,
            &rust_plist,
            &journal_path,
            || Err("stopped snapshot changed after the import report".into()),
        )
        .unwrap_err();

        assert_eq!(error.journal.state, "post-stop-snapshot-validation-failed");
        assert_eq!(error.journal.recovery_state, "node-restored");
        assert!(error.message.contains("未启动 RInBot"));
        let events = events.borrow();
        let node_stop = events
            .iter()
            .position(|event| event == "stop:com.local.qq-agent-plus")
            .unwrap();
        let node_restore = events
            .iter()
            .position(|event| event == "start:com.local.qq-agent-plus")
            .unwrap();
        assert!(node_stop < node_restore);
        assert!(!events.iter().any(|event| event == "start:com.local.rinbot"));
    }

    #[test]
    fn first_cutover_records_uncertain_node_state_without_starting_either_service() {
        let (temp, _, _, rust_plist, journal_path) = fixture();
        let node_data = temp.path().join("node-data");
        let rust_data = temp.path().join("rust-data");
        fs::create_dir(&node_data).unwrap();
        fs::create_dir(&rust_data).unwrap();
        let node_plist = temp.path().join("node.plist");
        fs::write(&node_plist, b"test node plist").unwrap();
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut driver = FakeDriver {
            events: events.clone(),
            running: true,
            candidate_ready: true,
            old_ready: true,
            journal_path: None,
            sabotage_journal_on_start: false,
            fail_candidate_stop: false,
            fail_node_stopped_check_after_stop: true,
            fail_rust_port_check_after_stop: false,
            stop_calls: 0,
        };

        let error = cutover_node_to_rust(
            &mut driver,
            &node_data,
            &rust_data,
            &node_plist,
            &rust_plist,
            &journal_path,
            || Ok(()),
        )
        .unwrap_err();

        assert_eq!(error.journal.state, "node-stop-not-confirmed");
        assert_eq!(error.journal.recovery_state, "node-state-uncertain");
        assert!(error.message.contains("未启动 RInBot"));
        let events = events.borrow();
        assert!(!events.iter().any(|event| event.starts_with("start:")));
        assert_eq!(
            serde_json::from_slice::<FirstCutoverJournal>(&fs::read(journal_path).unwrap())
                .unwrap()
                .recovery_state,
            "node-state-uncertain"
        );
    }

    #[test]
    fn first_cutover_candidate_failure_stops_rust_before_restoring_node() {
        let (temp, _, _, rust_plist, journal_path) = fixture();
        let node_data = temp.path().join("node-data");
        let rust_data = temp.path().join("rust-data");
        fs::create_dir(&node_data).unwrap();
        fs::create_dir(&rust_data).unwrap();
        let node_plist = temp.path().join("node.plist");
        fs::write(&node_plist, b"test node plist").unwrap();
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut driver = FakeDriver {
            events: events.clone(),
            running: true,
            candidate_ready: false,
            old_ready: true,
            journal_path: None,
            sabotage_journal_on_start: false,
            fail_candidate_stop: false,
            fail_node_stopped_check_after_stop: false,
            fail_rust_port_check_after_stop: false,
            stop_calls: 0,
        };

        let error = cutover_node_to_rust(
            &mut driver,
            &node_data,
            &rust_data,
            &node_plist,
            &rust_plist,
            &journal_path,
            || Ok(()),
        )
        .unwrap_err();

        assert_eq!(error.journal.state, "rust-start-failed");
        assert_eq!(error.journal.recovery_state, "node-restored");
        let events = events.borrow();
        let rust_start = events
            .iter()
            .position(|event| event == "start:com.local.rinbot")
            .unwrap();
        let rust_stop = events
            .iter()
            .position(|event| event == "stop:com.local.rinbot")
            .unwrap();
        let node_recheck = events
            .iter()
            .rposition(|event| event == "check:com.local.qq-agent-plus")
            .unwrap();
        assert!(rust_start < rust_stop && rust_stop < node_recheck);
        assert!(events.iter().any(|event| event == "node-stopped-check"));
    }

    #[test]
    fn first_cutover_never_restores_node_if_rust_stop_is_unconfirmed() {
        let (temp, _, _, rust_plist, journal_path) = fixture();
        let node_data = temp.path().join("node-data");
        let rust_data = temp.path().join("rust-data");
        fs::create_dir(&node_data).unwrap();
        fs::create_dir(&rust_data).unwrap();
        let node_plist = temp.path().join("node.plist");
        fs::write(&node_plist, b"test node plist").unwrap();
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut driver = FakeDriver {
            events: events.clone(),
            running: true,
            candidate_ready: false,
            old_ready: true,
            journal_path: None,
            sabotage_journal_on_start: false,
            fail_candidate_stop: true,
            fail_node_stopped_check_after_stop: false,
            fail_rust_port_check_after_stop: false,
            stop_calls: 0,
        };

        let error = cutover_node_to_rust(
            &mut driver,
            &node_data,
            &rust_data,
            &node_plist,
            &rust_plist,
            &journal_path,
            || Ok(()),
        )
        .unwrap_err();

        assert_eq!(error.journal.recovery_state, "recovery-failed");
        assert!(!events
            .borrow()
            .iter()
            .any(|event| event == "start:com.local.qq-agent-plus"));
        assert!(error.message.contains("避免双写"));
    }

    #[test]
    fn first_cutover_never_restores_node_if_rust_listener_remains_open() {
        let (temp, _, _, rust_plist, journal_path) = fixture();
        let node_data = temp.path().join("node-data");
        let rust_data = temp.path().join("rust-data");
        fs::create_dir(&node_data).unwrap();
        fs::create_dir(&rust_data).unwrap();
        let node_plist = temp.path().join("node.plist");
        fs::write(&node_plist, b"test node plist").unwrap();
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut driver = FakeDriver {
            events: events.clone(),
            running: true,
            candidate_ready: false,
            old_ready: true,
            journal_path: None,
            sabotage_journal_on_start: false,
            fail_candidate_stop: false,
            fail_node_stopped_check_after_stop: false,
            fail_rust_port_check_after_stop: true,
            stop_calls: 0,
        };

        let error = cutover_node_to_rust(
            &mut driver,
            &node_data,
            &rust_data,
            &node_plist,
            &rust_plist,
            &journal_path,
            || Ok(()),
        )
        .unwrap_err();

        assert_eq!(error.journal.state, "rust-start-failed");
        assert_eq!(error.journal.recovery_state, "recovery-failed");
        assert!(error.message.contains("避免双写"));
        let events = events.borrow();
        let rust_stop = events
            .iter()
            .rposition(|event| event == "stop:com.local.rinbot")
            .unwrap();
        let rust_port_check = events
            .iter()
            .rposition(|event| event == "rust-port-closed-check")
            .unwrap();
        assert!(rust_stop < rust_port_check);
        assert!(!events
            .iter()
            .any(|event| event == "start:com.local.qq-agent-plus"));
    }

    #[test]
    fn failed_candidate_restores_pointer_and_restarts_old_release() {
        let (temp, root, candidate, plist, journal_path) = fixture();
        let old = fs::canonicalize(temp.path().join("active")).unwrap();
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut driver = FakeDriver {
            events: events.clone(),
            running: true,
            candidate_ready: false,
            old_ready: true,
            journal_path: None,
            sabotage_journal_on_start: false,
            fail_candidate_stop: false,
            fail_node_stopped_check_after_stop: false,
            fail_rust_port_check_after_stop: false,
            stop_calls: 0,
        };
        let error = restart_with_rollback(
            &mut driver,
            &temp.path().join("active"),
            &candidate,
            &root,
            &plist,
            "http://127.0.0.1:3211/api/status",
            &journal_path,
        )
        .unwrap_err();
        assert!(error.message.contains("旧版本已自动恢复"));
        assert_eq!(fs::canonicalize(temp.path().join("active")).unwrap(), old);
        assert_eq!(error.journal.recovery_state, "old-service-restored");
        assert_eq!(
            events
                .borrow()
                .iter()
                .filter(|event| event.as_str() == "ready-check")
                .count(),
            2
        );
    }

    #[test]
    fn candidate_stop_failure_still_restores_pointer_without_starting_old_instance() {
        let (temp, root, candidate, plist, journal_path) = fixture();
        let active = temp.path().join("active");
        let old = fs::canonicalize(&active).unwrap();
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut driver = FakeDriver {
            events: events.clone(),
            running: true,
            candidate_ready: false,
            old_ready: true,
            journal_path: None,
            sabotage_journal_on_start: false,
            fail_candidate_stop: true,
            fail_node_stopped_check_after_stop: false,
            fail_rust_port_check_after_stop: false,
            stop_calls: 0,
        };

        let error = restart_with_rollback(
            &mut driver,
            &active,
            &candidate,
            &root,
            &plist,
            "http://127.0.0.1:3211/api/status",
            &journal_path,
        )
        .unwrap_err();

        assert_eq!(fs::canonicalize(&active).unwrap(), old);
        assert_eq!(error.journal.recovery_state, "old-service-recovery-failed");
        assert!(error.message.contains("指针已恢复旧版"));
        assert_eq!(
            events
                .borrow()
                .iter()
                .filter(|event| event.starts_with("start:"))
                .count(),
            1,
            "do not start the old service while candidate stop is unconfirmed"
        );
    }

    #[test]
    fn restart_rejects_candidate_outside_retained_release_root() {
        let (temp, root, _, _, _) = fixture();
        let outside = temp.path().join("outside");
        fs::write(&outside, b"outside").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&outside, fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(validate_restart_paths(&temp.path().join("active"), &outside, &root).is_err());
    }

    #[test]
    fn first_cutover_requires_active_symlink_to_preselect_candidate() {
        let (temp, root, candidate, _, _) = fixture();
        fs::remove_file(temp.path().join("active")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&candidate, temp.path().join("active")).unwrap();
        assert_eq!(
            validate_first_cutover_candidate(&temp.path().join("active"), &candidate, &root)
                .unwrap(),
            fs::canonicalize(candidate).unwrap()
        );
        let other = root.join("v1/RInBot");
        assert!(
            validate_first_cutover_candidate(&temp.path().join("active"), &other, &root).is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn launchctl_unloaded_check_only_accepts_the_known_missing_service_code() {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            launchctl_target_loaded(std::process::ExitStatus::from_raw(0)).unwrap(),
            true
        );
        assert_eq!(
            launchctl_target_loaded(std::process::ExitStatus::from_raw(113 << 8)).unwrap(),
            false
        );
        assert!(launchctl_target_loaded(std::process::ExitStatus::from_raw(1 << 8)).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "loads and unloads a uniquely named temporary LaunchAgent; run explicitly for the isolated launchd drill"]
    fn isolated_launchd_driver_bootstraps_checks_and_unloads_temporary_agent() {
        let temp = tempdir().unwrap();
        let label = format!(
            "com.local.rinbot.cutover-drill.{}.{}",
            std::process::id(),
            now_ms()
        );
        let plist_path = temp.path().join("isolated-launch-agent.plist");
        let stdout_path = temp.path().join("agent.stdout.log");
        let stderr_path = temp.path().join("agent.stderr.log");
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\"><dict>\n\
             <key>Label</key><string>{label}</string>\n\
             <key>ProgramArguments</key><array><string>/bin/sleep</string><string>60</string></array>\n\
             <key>RunAtLoad</key><true/><key>KeepAlive</key><false/>\n\
             <key>StandardOutPath</key><string>{}</string>\n\
             <key>StandardErrorPath</key><string>{}</string>\n\
             </dict></plist>",
            stdout_path.display(),
            stderr_path.display()
        );
        fs::write(&plist_path, plist).unwrap();

        let result = (|| {
            let mut driver = LaunchctlDriver::new()?;
            driver.ensure_stopped(&label)?;
            driver.start(&label, &plist_path)?;
            driver.ensure_running(&label)?;
            driver.stop(&label, STOP_BUDGET)?;
            driver.ensure_stopped(&label)
        })();

        if result.is_err() {
            if let Ok(mut driver) = LaunchctlDriver::new() {
                let _ = driver.stop(&label, STOP_BUDGET);
            }
        }
        assert!(result.is_ok(), "isolated launchd drill failed: {result:?}");
    }

    #[test]
    fn readiness_token_uses_cli_precedence_and_rejects_duplicate_or_injected_headers() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("auth.plist");
        let write = |arguments: &[&str]| {
            let mut values = plist::Dictionary::new();
            values.insert(
                "ProgramArguments".into(),
                plist::Value::Array(
                    arguments
                        .iter()
                        .map(|value| plist::Value::String((*value).into()))
                        .collect(),
                ),
            );
            let mut environment = plist::Dictionary::new();
            environment.insert(
                "QQBOTD_API_TOKEN".into(),
                plist::Value::String("environment-fixture".into()),
            );
            values.insert(
                "EnvironmentVariables".into(),
                plist::Value::Dictionary(environment),
            );
            plist::Value::Dictionary(values).to_file_xml(&path).unwrap();
        };
        write(&["RInBot"]);
        assert_eq!(
            rust_api_token_from_plist(&path).unwrap().as_deref(),
            Some("environment-fixture")
        );
        write(&["RInBot", "--api-token", "cli-fixture"]);
        assert_eq!(
            rust_api_token_from_plist(&path).unwrap().as_deref(),
            Some("cli-fixture")
        );
        write(&["RInBot", "--api-token=equals-fixture"]);
        assert_eq!(
            rust_api_token_from_plist(&path).unwrap().as_deref(),
            Some("equals-fixture")
        );
        write(&["RInBot", "--api-token", "a", "--api-token=b"]);
        assert!(rust_api_token_from_plist(&path)
            .unwrap_err()
            .contains("重复"));
        write(&["RInBot", "--api-token", "fixture\nX-Injected: yes"]);
        assert!(rust_api_token_from_plist(&path)
            .unwrap_err()
            .contains("字符"));
    }

    #[test]
    fn readiness_authenticates_local_api_and_never_sends_token_to_other_hosts() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut buffer = [0_u8; 512];
                let received = stream.read(&mut buffer).unwrap();
                assert!(received > 0 && request.len() + received <= 8_192);
                request.extend_from_slice(&buffer[..received]);
            }
            let authenticated = String::from_utf8(request)
                .unwrap()
                .lines()
                .any(|line| line == "Authorization: Bearer readiness-fixture-token");
            let body = r#"{"mode":"active","onebot_connected":true,"core_ready":true}"#;
            let status = if authenticated {
                "200 OK"
            } else {
                "401 Unauthorized"
            };
            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            authenticated
        });
        let mut driver = LaunchctlDriver {
            domain: "unused-test-domain".into(),
            api_token: Some("readiness-fixture-token".into()),
        };
        driver
            .wait_ready(
                &format!("http://{address}/api/status"),
                Duration::from_secs(3),
            )
            .unwrap();
        assert!(server.join().unwrap());
        assert!(driver
            .wait_ready("http://example.com/api/status", Duration::from_secs(1))
            .is_err());
        assert!(driver
            .wait_ready(
                "http://127.0.0.1/api/status?token=test",
                Duration::from_secs(1)
            )
            .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn operation_lock_serializes_service_control_across_processes() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("operation.lock");
        let uid = unsafe { libc::getuid() };
        let first = OperationLock::acquire_at(&path, uid).unwrap();
        assert!(first.is_held());
        assert!(OperationLock::acquire_at(&path, uid).is_err());
        drop(first);
        assert!(OperationLock::acquire_at(&path, uid).is_ok());
    }

    #[test]
    fn restart_requires_running_service_and_does_not_touch_pointer_when_not_loaded() {
        let (temp, root, candidate, plist, journal_path) = fixture();
        let active = temp.path().join("active");
        let old = fs::canonicalize(&active).unwrap();
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut driver = FakeDriver {
            events: events.clone(),
            running: false,
            candidate_ready: true,
            old_ready: true,
            journal_path: None,
            sabotage_journal_on_start: false,
            fail_candidate_stop: false,
            fail_node_stopped_check_after_stop: false,
            fail_rust_port_check_after_stop: false,
            stop_calls: 0,
        };
        let error = restart_with_rollback(
            &mut driver,
            &active,
            &candidate,
            &root,
            &plist,
            "http://127.0.0.1:3211/api/status",
            &journal_path,
        )
        .unwrap_err();
        assert!(error.message.contains("当前 LaunchAgent 已运行"));
        assert_eq!(fs::canonicalize(active).unwrap(), old);
        assert_eq!(events.borrow().as_slice(), ["check:com.local.rinbot"]);
        assert_eq!(
            serde_json::from_slice::<RestartJournal>(&fs::read(journal_path).unwrap())
                .unwrap()
                .state,
            "precondition-failed"
        );
    }

    #[test]
    fn restart_never_overwrites_an_existing_operation_journal() {
        let (temp, root, candidate, plist, journal_path) = fixture();
        fs::write(&journal_path, b"preserve me").unwrap();
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut driver = FakeDriver {
            events: events.clone(),
            running: true,
            candidate_ready: true,
            old_ready: true,
            journal_path: None,
            sabotage_journal_on_start: false,
            fail_candidate_stop: false,
            fail_node_stopped_check_after_stop: false,
            fail_rust_port_check_after_stop: false,
            stop_calls: 0,
        };
        let old = fs::canonicalize(temp.path().join("active")).unwrap();
        assert!(restart_with_rollback(
            &mut driver,
            &temp.path().join("active"),
            &candidate,
            &root,
            &plist,
            "http://127.0.0.1:3211/api/status",
            &journal_path,
        )
        .is_err());
        assert_eq!(fs::read(&journal_path).unwrap(), b"preserve me");
        assert_eq!(fs::canonicalize(temp.path().join("active")).unwrap(), old);
        assert!(events.borrow().is_empty());
    }

    #[test]
    fn failed_success_journal_rolls_back_candidate_after_readiness() {
        let (temp, root, candidate, plist, journal_path) = fixture();
        let old = fs::canonicalize(temp.path().join("active")).unwrap();
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut driver = FakeDriver {
            events: events.clone(),
            running: true,
            candidate_ready: true,
            old_ready: true,
            journal_path: Some(journal_path),
            sabotage_journal_on_start: true,
            fail_candidate_stop: false,
            fail_node_stopped_check_after_stop: false,
            fail_rust_port_check_after_stop: false,
            stop_calls: 0,
        };
        let error = restart_with_rollback(
            &mut driver,
            &temp.path().join("active"),
            &candidate,
            &root,
            &plist,
            "http://127.0.0.1:3211/api/status",
            &temp.path().join("ops/restart.json"),
        )
        .unwrap_err();
        assert!(error.message.contains("成功记录无法持久化"));
        assert_eq!(error.journal.recovery_state, "old-service-restored");
        assert_eq!(fs::canonicalize(temp.path().join("active")).unwrap(), old);
        assert_eq!(
            events
                .borrow()
                .iter()
                .filter(|event| event.starts_with("start:"))
                .count(),
            2
        );
    }
}
