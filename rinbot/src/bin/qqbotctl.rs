use clap::{Parser, Subcommand};
use plist::Value as PlistValue;
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

#[path = "qqbotctl/switch.rs"]
mod switch;

#[derive(Debug, Parser)]
#[command(
    name = "qqbotctl",
    about = "RInBot cutover preflight and gated Rust release restart"
)]
struct Args {
    #[command(subcommand)]
    action: Action,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// Review service bindings offline and print structured evidence; never load either service.
    ReviewLaunchd {
        #[arg(long)]
        node_data_dir: PathBuf,
        #[arg(long)]
        rust_data_dir: PathBuf,
        #[arg(long)]
        node_plist: PathBuf,
        #[arg(long)]
        rust_plist: PathBuf,
    },
    /// Check static cutover gates without contacting or modifying either service.
    Preflight {
        /// Existing Node data directory (must remain separate from Rust data).
        #[arg(long)]
        node_data_dir: PathBuf,
        /// Prepared Rust data directory, created from the verified stopped-Node snapshot.
        #[arg(long)]
        rust_data_dir: PathBuf,
        /// Operator-maintained JSON evidence report; every required gate must be true.
        #[arg(long)]
        readiness_report: PathBuf,
        /// Node LaunchAgent plist expected to be used for rollback.
        #[arg(long)]
        node_plist: PathBuf,
        /// RInBot LaunchAgent plist expected for the candidate service.
        #[arg(long)]
        rust_plist: PathBuf,
    },
    /// Atomically switch a running Rust LaunchAgent between retained releases.
    RestartRust {
        #[arg(long)]
        node_data_dir: PathBuf,
        #[arg(long)]
        rust_data_dir: PathBuf,
        #[arg(long)]
        readiness_report: PathBuf,
        #[arg(long)]
        node_plist: PathBuf,
        #[arg(long)]
        rust_plist: PathBuf,
        /// Stable symlink used as ProgramArguments[0] in the RInBot plist.
        #[arg(long)]
        active_link: PathBuf,
        /// Candidate executable already built inside the retained release root.
        #[arg(long)]
        candidate: PathBuf,
        #[arg(long)]
        release_root: PathBuf,
        /// Durable operation record, outside the release directory.
        #[arg(long)]
        journal: PathBuf,
        /// Execute after every static gate passes; without this flag only print the plan.
        #[arg(long)]
        apply: bool,
        /// Exact operator confirmation required with --apply.
        #[arg(long)]
        confirm: Option<String>,
    },
    /// Perform the first gated Node-to-Rust handoff with automatic Node recovery.
    CutoverNodeToRust {
        #[arg(long)]
        node_data_dir: PathBuf,
        #[arg(long)]
        rust_data_dir: PathBuf,
        #[arg(long)]
        readiness_report: PathBuf,
        #[arg(long)]
        node_plist: PathBuf,
        #[arg(long)]
        rust_plist: PathBuf,
        /// Stable symlink used as ProgramArguments[0] in the RInBot plist.
        #[arg(long)]
        active_link: PathBuf,
        /// Candidate executable already staged in the retained release root.
        #[arg(long)]
        candidate: PathBuf,
        #[arg(long)]
        release_root: PathBuf,
        /// Durable operation record, outside the release directory.
        #[arg(long)]
        journal: PathBuf,
        /// Execute only after all evidence gates pass; otherwise print a plan.
        #[arg(long)]
        apply: bool,
        /// Exact operator confirmation required with --apply.
        #[arg(long)]
        confirm: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
struct ReadinessReport {
    gates: BTreeMap<String, GateEvidence>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GateEvidence {
    passed: bool,
    evidence_file: PathBuf,
    sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GateEvidenceDocument {
    format_version: u32,
    gate: String,
    passed: bool,
    recorded_at: String,
    #[serde(default)]
    checks: BTreeMap<String, bool>,
    #[serde(default)]
    metrics: BTreeMap<String, f64>,
    #[serde(default)]
    enabled_features: Vec<String>,
    #[serde(default)]
    unmapped_enabled_features: Vec<String>,
    #[serde(default)]
    data_blockers: Vec<String>,
    #[serde(default)]
    unresolved_items: Vec<String>,
    #[serde(default)]
    failed_records: Vec<String>,
    #[serde(default)]
    node_config_sha256: Option<String>,
    #[serde(default)]
    unknown_outbox_sha256: Option<String>,
    #[serde(default)]
    migration_report_sha256: Option<String>,
    #[serde(default)]
    rust_binary_sha256: Option<String>,
    #[serde(default)]
    ui_bundle_sha256: Option<String>,
    #[serde(default)]
    node_plist_sha256: Option<String>,
    #[serde(default)]
    rust_plist_sha256: Option<String>,
    #[serde(default)]
    media_fixture_sha256: Option<String>,
    #[serde(default)]
    stability_samples_file: Option<PathBuf>,
    #[serde(default)]
    stability_samples_sha256: Option<String>,
}

const REQUIRED_GATES: &[&str] = &[
    "enabled_features_mapped",
    "node_snapshot_import_reconciled",
    "unknown_outbox_reconciled",
    "napcat_qq_acceptance",
    "performance_acceptance",
    "stability_24h",
    "ten_controlled_restarts",
    "rollback_drill",
    "first_node_to_rust_cutover_drill",
    "launchd_configs_reviewed",
];

#[derive(Debug)]
struct Check {
    name: &'static str,
    passed: bool,
    detail: String,
}

fn main() {
    let Args { action } = Args::parse();
    match action {
        Action::ReviewLaunchd {
            node_data_dir,
            rust_data_dir,
            node_plist,
            rust_plist,
        } => match review_launchd(&node_data_dir, &rust_data_dir, &node_plist, &rust_plist) {
            Ok(document) => println!("{}", serde_json::to_string_pretty(&document).unwrap()),
            Err(reason) => {
                eprintln!("LaunchAgent 离线审阅拒绝：{reason}；未执行服务操作");
                std::process::exit(2);
            }
        },
        Action::Preflight {
            node_data_dir,
            rust_data_dir,
            readiness_report,
            node_plist,
            rust_plist,
        } => {
            let checks = preflight(
                &node_data_dir,
                &rust_data_dir,
                &readiness_report,
                &node_plist,
                &rust_plist,
            );
            let blockers: Vec<_> = checks.iter().filter(|check| !check.passed).collect();
            println!("RInBot 切换离线预检（只读；未连接服务）");
            for check in &checks {
                println!(
                    "{} {} — {}",
                    if check.passed { "通过" } else { "阻断" },
                    check.name,
                    check.detail
                );
            }
            if blockers.is_empty() {
                println!("预检通过。此结果不是切换授权，也未执行任何服务操作。");
            } else {
                println!("共 {} 项阻断；不可进行生产切换。", blockers.len());
                std::process::exit(2);
            }
        }
        Action::RestartRust {
            node_data_dir,
            rust_data_dir,
            readiness_report,
            node_plist,
            rust_plist,
            active_link,
            candidate,
            release_root,
            journal,
            apply,
            confirm,
        } => {
            let checks = preflight(
                &node_data_dir,
                &rust_data_dir,
                &readiness_report,
                &node_plist,
                &rust_plist,
            );
            let blockers: Vec<_> = checks.iter().filter(|check| !check.passed).collect();
            println!("RInBot Rust 版本切换预检");
            for check in &checks {
                println!(
                    "{} {} — {}",
                    if check.passed { "通过" } else { "阻断" },
                    check.name,
                    check.detail
                );
            }
            let switch_paths =
                switch::validate_restart_paths(&active_link, &candidate, &release_root);
            let active_binding = inspect_rust_active_link_binding(&rust_plist, &active_link);
            if let Err(error) = &switch_paths {
                println!("阻断 版本指针 — {error}");
            }
            if let Err(error) = &active_binding {
                println!("阻断 LaunchAgent 版本指针绑定 — {error}");
            }
            if !blockers.is_empty() || switch_paths.is_err() || active_binding.is_err() {
                eprintln!(
                    "静态门槛未全部通过；没有执行任何服务操作。共 {} 项阻断。",
                    blockers.len()
                        + usize::from(switch_paths.is_err())
                        + usize::from(active_binding.is_err())
                );
                std::process::exit(2);
            }
            let (_, previous, candidate) = switch_paths.unwrap();
            println!("当前版：{}", previous.display());
            println!("候选版：{}", candidate.display());
            println!("目标健康检查：http://127.0.0.1:3211/api/status（active + OneBot + core）");
            if !apply {
                println!("仅预览：未查询/停止/启动 LaunchAgent，未改动版本指针。");
                return;
            }
            if confirm.as_deref() != Some("RESTART-RUST-AFTER-GATES") {
                eprintln!("拒绝执行：--apply 必须同时提供 --confirm RESTART-RUST-AFTER-GATES");
                std::process::exit(2);
            }
            let _operation_lock = match switch::OperationLock::acquire() {
                Ok(lock) => lock,
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(2);
                }
            };
            let locked_blockers = first_cutover_static_blockers(
                &node_data_dir,
                &rust_data_dir,
                &readiness_report,
                &node_plist,
                &rust_plist,
                &active_link,
                &candidate,
                &release_root,
            );
            if !locked_blockers.is_empty() {
                eprintln!("加锁后静态门槛复核失败；没有执行任何服务操作：");
                for blocker in locked_blockers {
                    eprintln!("阻断 {blocker}");
                }
                std::process::exit(2);
            }
            let mut driver = match switch::LaunchctlDriver::for_rust_plist(&rust_plist) {
                Ok(driver) => driver,
                Err(error) => {
                    eprintln!("无法准备 LaunchAgent 控制器：{error}");
                    std::process::exit(2);
                }
            };
            match switch::restart_with_rollback(
                &mut driver,
                &active_link,
                &candidate,
                &release_root,
                &rust_plist,
                "http://127.0.0.1:3211/api/status",
                &journal,
            ) {
                Ok(record) => println!("重启成功：{}", serde_json::to_string(&record).unwrap()),
                Err(error) => {
                    eprintln!("{}", error.message);
                    eprintln!(
                        "operation={}",
                        serde_json::to_string(&error.journal).unwrap()
                    );
                    std::process::exit(3);
                }
            }
        }
        Action::CutoverNodeToRust {
            node_data_dir,
            rust_data_dir,
            readiness_report,
            node_plist,
            rust_plist,
            active_link,
            candidate,
            release_root,
            journal,
            apply,
            confirm,
        } => {
            let checks = preflight(
                &node_data_dir,
                &rust_data_dir,
                &readiness_report,
                &node_plist,
                &rust_plist,
            );
            let blockers: Vec<_> = checks.iter().filter(|check| !check.passed).collect();
            println!("首次 Node→RInBot 切换预检");
            for check in &checks {
                println!(
                    "{} {} — {}",
                    if check.passed { "通过" } else { "阻断" },
                    check.name,
                    check.detail
                );
            }
            let candidate_path =
                switch::validate_first_cutover_candidate(&active_link, &candidate, &release_root);
            let active_binding = inspect_rust_active_link_binding(&rust_plist, &active_link);
            if let Err(error) = &candidate_path {
                println!("阻断 候选版本 — {error}");
            }
            if let Err(error) = &active_binding {
                println!("阻断 LaunchAgent 版本指针绑定 — {error}");
            }
            if !blockers.is_empty() || candidate_path.is_err() || active_binding.is_err() {
                eprintln!(
                    "静态门槛未全部通过；没有执行任何服务操作。共 {} 项阻断。",
                    blockers.len()
                        + usize::from(candidate_path.is_err())
                        + usize::from(active_binding.is_err())
                );
                std::process::exit(2);
            }
            println!("候选程序：{}", candidate_path.unwrap().display());
            println!("要求：先导出 Node 进程内唤醒快照，再停写并完成最终快照/导入；复核 3210 端口关闭后启动 RInBot；失败时先确认 Rust 停止再恢复 Node");
            if !apply {
                println!("仅预览：未查询/停止/启动 LaunchAgent，未触碰 Node 或版本指针。没有任何实际写操作。");
                return;
            }
            if confirm.as_deref() != Some("CUTOVER-NODE-TO-RUST-AFTER-GATES") {
                eprintln!(
                    "拒绝执行：--apply 必须同时提供 --confirm CUTOVER-NODE-TO-RUST-AFTER-GATES"
                );
                std::process::exit(2);
            }
            let _operation_lock = match switch::OperationLock::acquire() {
                Ok(lock) => lock,
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(2);
                }
            };
            let locked_blockers = first_cutover_static_blockers(
                &node_data_dir,
                &rust_data_dir,
                &readiness_report,
                &node_plist,
                &rust_plist,
                &active_link,
                &candidate,
                &release_root,
            );
            if !locked_blockers.is_empty() {
                eprintln!("加锁后首次切换静态门槛复核失败；未执行任何服务操作：");
                for blocker in locked_blockers {
                    eprintln!("阻断 {blocker}");
                }
                std::process::exit(2);
            }
            let mut driver = match switch::LaunchctlDriver::for_rust_plist(&rust_plist) {
                Ok(driver) => driver,
                Err(error) => {
                    eprintln!("无法准备 LaunchAgent 控制器：{error}");
                    std::process::exit(2);
                }
            };
            match switch::cutover_node_to_rust(
                &mut driver,
                &node_data_dir,
                &rust_data_dir,
                &node_plist,
                &rust_plist,
                &journal,
                || {
                    let blockers = first_cutover_static_blockers(
                        &node_data_dir,
                        &rust_data_dir,
                        &readiness_report,
                        &node_plist,
                        &rust_plist,
                        &active_link,
                        &candidate,
                        &release_root,
                    );
                    if blockers.is_empty() {
                        Ok(())
                    } else {
                        Err(blockers.join("；"))
                    }
                },
            ) {
                Ok(record) => println!("首次切换成功：{}", serde_json::to_string(&record).unwrap()),
                Err(error) => {
                    eprintln!("{}", error.message);
                    eprintln!(
                        "operation={}",
                        serde_json::to_string(&error.journal).unwrap()
                    );
                    std::process::exit(3);
                }
            }
        }
    }
}

fn inspect_rust_active_link_binding(plist_path: &Path, active_link: &Path) -> Result<(), String> {
    let plist = read_launch_agent(plist_path)?;
    let executable = string_array(&plist, "ProgramArguments")
        .and_then(|arguments| arguments.first().copied())
        .ok_or_else(|| "RInBot ProgramArguments 缺少可执行路径".to_owned())?;
    if Path::new(executable) != active_link {
        return Err("RInBot ProgramArguments[0] 必须精确指向指定的活动版本符号链接".into());
    }
    Ok(())
}

fn first_cutover_static_blockers(
    node_data_dir: &Path,
    rust_data_dir: &Path,
    readiness_report: &Path,
    node_plist: &Path,
    rust_plist: &Path,
    active_link: &Path,
    candidate: &Path,
    release_root: &Path,
) -> Vec<String> {
    let mut blockers = preflight(
        node_data_dir,
        rust_data_dir,
        readiness_report,
        node_plist,
        rust_plist,
    )
    .into_iter()
    .filter(|check| !check.passed)
    .map(|check| format!("{} — {}", check.name, check.detail))
    .collect::<Vec<_>>();
    if let Err(error) =
        switch::validate_first_cutover_candidate(active_link, candidate, release_root)
    {
        blockers.push(format!("候选版本 — {error}"));
    }
    if let Err(error) = inspect_rust_active_link_binding(rust_plist, active_link) {
        blockers.push(format!("LaunchAgent 版本指针绑定 — {error}"));
    }
    blockers
}

fn preflight(
    node_data_dir: &Path,
    rust_data_dir: &Path,
    readiness_report: &Path,
    node_plist: &Path,
    rust_plist: &Path,
) -> Vec<Check> {
    let mut checks = Vec::new();
    let node = canonical_existing_dir(node_data_dir);
    let rust = canonical_existing_dir(rust_data_dir);
    checks.push(Check {
        name: "数据目录隔离",
        passed: match (&node, &rust) {
            (Some(node), Some(rust)) => !node.starts_with(rust) && !rust.starts_with(node),
            _ => false,
        },
        detail: match (&node, &rust) {
            (Some(node), Some(rust)) if !node.starts_with(rust) && !rust.starts_with(node) => {
                "Node 与 RInBot 数据目录均存在且互不嵌套".into()
            }
            (Some(_), Some(_)) => "Node 与 RInBot 数据目录重叠".into(),
            _ => "两个数据目录都必须已存在且可规范化".into(),
        },
    });

    for (name, path, kind) in [
        ("Node 回滚 LaunchAgent", node_plist, "node"),
        ("RInBot LaunchAgent", rust_plist, "rust"),
    ] {
        let result = match (kind, &node, &rust) {
            ("node", Some(node_dir), _) => inspect_node_plist(path, node_dir),
            ("rust", _, Some(rust_dir)) => inspect_rust_plist(path, rust_dir),
            _ => Err("数据目录不可规范化，无法验证 LaunchAgent 绑定".to_owned()),
        };
        checks.push(Check {
            name,
            passed: result.is_ok(),
            detail: result.unwrap_or_else(|reason| reason),
        });
    }

    let isolation = inspect_distinct_service_ports_and_logs(node_data_dir, node_plist, rust_plist);
    checks.push(Check {
        name: "服务端口与日志隔离",
        passed: isolation.is_ok(),
        detail: isolation.unwrap_or_else(|reason| reason),
    });

    match fs::read(readiness_report)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ReadinessReport>(&bytes).ok())
    {
        Some(report) => {
            for &gate in REQUIRED_GATES {
                let evidence = report.gates.get(gate);
                let (passed, detail) = match evidence {
                    Some(evidence) if !evidence.passed => (false, "验收报告未通过".into()),
                    Some(evidence) => {
                        match verify_evidence_with_launch_agents(
                            readiness_report,
                            gate,
                            evidence,
                            node_data_dir,
                            rust_data_dir,
                            Some((node_plist, rust_plist)),
                        ) {
                            Ok(()) => (true, "通过标记及证据文件 SHA-256 已核对".into()),
                            Err(reason) => (false, reason),
                        }
                    }
                    None => (false, "报告缺少此项".into()),
                };
                checks.push(Check {
                    name: gate_label(gate),
                    passed,
                    detail,
                });
            }
        }
        None => checks.push(Check {
            name: "切换验收报告",
            passed: false,
            detail: "文件不可读或 JSON 格式无效".into(),
        }),
    }
    checks
}

fn read_launch_agent(path: &Path) -> Result<PlistValue, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| "配置文件不存在或不可读；此预检不会创建或加载它".to_owned())?;
    if !metadata.file_type().is_file() || metadata.len() > 1024 * 1024 {
        return Err("LaunchAgent 必须是普通文件，不能是目录或符号链接".into());
    }
    PlistValue::from_file(path).map_err(|_| "LaunchAgent plist 无法解析".into())
}

/// This gate covers static service configuration only. It does not attest that
/// migration, QQ acceptance, or a production handoff has occurred.
fn review_launchd(
    node_data_dir: &Path,
    rust_data_dir: &Path,
    node_plist: &Path,
    rust_plist: &Path,
) -> Result<serde_json::Value, String> {
    let node =
        canonical_existing_dir(node_data_dir).ok_or_else(|| "Node 数据目录必须存在".to_owned())?;
    let rust = canonical_existing_dir(rust_data_dir)
        .ok_or_else(|| "RInBot 数据目录必须存在".to_owned())?;
    if node.starts_with(&rust) || rust.starts_with(&node) {
        return Err("Node 与 RInBot 数据目录不能重叠".into());
    }
    // Bind the exact bytes before inspection and verify they stayed unchanged.
    // Nothing in this function contacts launchd or opens a listening port.
    let node_sha = sha256_regular_file(node_plist, 1024 * 1024)?;
    let rust_sha = sha256_regular_file(rust_plist, 1024 * 1024)?;
    let config_path = node.join("config.json");
    let config_sha = sha256_regular_file(&config_path, 4 * 1024 * 1024)?;
    inspect_node_plist(node_plist, &node)?;
    inspect_rust_plist(rust_plist, &rust)?;
    inspect_distinct_service_ports_and_logs(&node, node_plist, rust_plist)?;
    let binary_sha = rust_binary_sha256(rust_plist)?;
    let ui_sha = rust_ui_bundle_sha256(rust_plist)?;
    if node_sha != sha256_regular_file(node_plist, 1024 * 1024)?
        || rust_sha != sha256_regular_file(rust_plist, 1024 * 1024)?
        || config_sha != sha256_regular_file(&config_path, 4 * 1024 * 1024)?
    {
        return Err("配置在审阅期间发生变化，请重新审阅".into());
    }
    Ok(serde_json::json!({
        "format_version": 1,
        "gate": "launchd_configs_reviewed",
        "passed": true,
        "recorded_at": time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|_| "无法记录审阅时间".to_owned())?,
        "checks": {
            "node_label_program_and_data_binding_verified": true,
            "rust_label_program_and_data_binding_verified": true,
            "rust_ui_entrypoint_bound_and_present": true,
            "distinct_ports_and_logs_verified": true,
            "keepalive_and_single_writer_policy_verified": true,
            "offline_preflight_is_non_mutating": true
        },
        "node_config_sha256": config_sha,
        "node_plist_sha256": node_sha,
        "rust_plist_sha256": rust_sha,
        "rust_binary_sha256": binary_sha,
        "ui_bundle_sha256": ui_sha
    }))
}

fn dictionary<'a>(value: &'a PlistValue, key: &str) -> Option<&'a plist::Dictionary> {
    value.as_dictionary()?.get(key)?.as_dictionary()
}

fn string_field<'a>(value: &'a PlistValue, key: &str) -> Option<&'a str> {
    value.as_dictionary()?.get(key)?.as_string()
}

fn string_array<'a>(value: &'a PlistValue, key: &str) -> Option<Vec<&'a str>> {
    value
        .as_dictionary()?
        .get(key)?
        .as_array()?
        .iter()
        .map(PlistValue::as_string)
        .collect()
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn matching_existing_directory(actual: &str, expected: &Path) -> bool {
    Path::new(actual)
        .is_absolute()
        .then(|| fs::canonicalize(actual).ok())
        .flatten()
        .is_some_and(|actual| Some(actual) == canonical_existing_dir(expected))
}

fn lifecycle_and_log_paths_valid(plist: &PlistValue) -> bool {
    let run_at_load = plist
        .as_dictionary()
        .and_then(|values| values.get("RunAtLoad"))
        .and_then(PlistValue::as_boolean)
        == Some(true);
    let keep_alive = plist
        .as_dictionary()
        .and_then(|values| values.get("KeepAlive"))
        .is_some_and(|value| {
            value.as_boolean() == Some(true)
                || value
                    .as_dictionary()
                    .and_then(|settings| settings.get("SuccessfulExit"))
                    .and_then(PlistValue::as_boolean)
                    == Some(false)
        });
    let stdout = string_field(plist, "StandardOutPath");
    let stderr = string_field(plist, "StandardErrorPath");
    run_at_load
        && keep_alive
        && stdout.is_some_and(|path| Path::new(path).is_absolute())
        && stdout != stderr
}

fn inspect_node_plist(path: &Path, node_data_dir: &Path) -> Result<String, String> {
    let plist = read_launch_agent(path)?;
    if !lifecycle_and_log_paths_valid(&plist) {
        return Err(
            "Node plist 必须配置 RunAtLoad、异常退出 KeepAlive 及不同的绝对日志路径".into(),
        );
    }
    if string_field(&plist, "Label") != Some("com.local.qq-agent-plus") {
        return Err("Node 回滚项的 Label 必须为 com.local.qq-agent-plus".into());
    }
    let arguments = string_array(&plist, "ProgramArguments")
        .filter(|arguments| arguments.len() >= 2)
        .ok_or_else(|| "Node ProgramArguments 缺失或格式无效".to_owned())?;
    if !Path::new(arguments[0]).is_absolute()
        || Path::new(arguments[0])
            .file_name()
            .and_then(|name| name.to_str())
            != Some("node")
        || !is_executable_file(Path::new(arguments[0]))
    {
        return Err("Node 启动程序必须是存在的绝对可执行文件".into());
    }
    let working_directory = string_field(&plist, "WorkingDirectory")
        .filter(|directory| Path::new(directory).is_absolute())
        .ok_or_else(|| "Node WorkingDirectory 必须是绝对路径".to_owned())?;
    let script = Path::new(arguments[1]);
    let script = if script.is_absolute() {
        script.to_path_buf()
    } else {
        Path::new(working_directory).join(script)
    };
    if !script.is_file() || !script.ends_with("src/server.js") {
        return Err("Node ProgramArguments 必须指向当前项目的 src/server.js".into());
    }
    let data_dir = dictionary(&plist, "EnvironmentVariables")
        .and_then(|environment| environment.get("QQ_AGENT_DATA_DIR"))
        .and_then(PlistValue::as_string)
        .ok_or_else(|| "Node EnvironmentVariables 缺少 QQ_AGENT_DATA_DIR".to_owned())?;
    if !matching_existing_directory(data_dir, node_data_dir) {
        return Err("Node QQ_AGENT_DATA_DIR 与指定生产数据目录不一致".into());
    }
    Ok(
        "plist 已解析；Label、Node 程序、server.js 与生产数据目录绑定均匹配（未执行服务操作）"
            .into(),
    )
}

fn inspect_node_console_port(node_data_dir: &Path) -> Result<u16, String> {
    let config_path = node_data_dir.join("config.json");
    let config: serde_json::Value = serde_json::from_reader(
        File::open(config_path).map_err(|_| "Node config.json 不存在或不可读".to_owned())?,
    )
    .map_err(|_| "Node config.json 格式无效".to_owned())?;
    let server = config
        .get("server")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "Node server 配置缺失或格式无效".to_owned())?;
    let host = server
        .get("host")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("127.0.0.1");
    if !matches!(host, "127.0.0.1" | "localhost" | "::1") {
        return Err("Node 控制台必须仅绑定本机回环地址".into());
    }
    let port = server
        .get("port")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(3210);
    if !(1..=u16::MAX as u64).contains(&port) {
        return Err("Node 控制台端口必须在 1–65535 范围内".into());
    }
    let strict_port = server
        .get("strictPort")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let last_port = if strict_port {
        port
    } else {
        port.saturating_add(9).min(u16::MAX as u64)
    };
    if (port..=last_port).contains(&3211) {
        return Err("Node 控制台端口或其自动回退范围与 RInBot 3211 冲突".into());
    }
    // The controller's stopped-writer probe and rollback readiness use this
    // exact address. Accepting another nonconflicting port would verify the
    // wrong listener and could make an otherwise valid rollback unobservable.
    if host != "127.0.0.1" || port != 3210 || !strict_port {
        return Err("受控切换要求 Node 固定监听 127.0.0.1:3210 且 strictPort=true；否则停止确认和回滚健康检查地址不匹配".into());
    }
    Ok(port as u16)
}

fn inspect_distinct_service_ports_and_logs(
    node_data_dir: &Path,
    node_plist_path: &Path,
    rust_plist_path: &Path,
) -> Result<String, String> {
    let node_port = inspect_node_console_port(node_data_dir)?;
    let node_plist = read_launch_agent(node_plist_path)?;
    let rust_plist = read_launch_agent(rust_plist_path)?;
    let mut paths = Vec::with_capacity(4);
    for (service, plist) in [("Node", &node_plist), ("RInBot", &rust_plist)] {
        for field in ["StandardOutPath", "StandardErrorPath"] {
            let raw = string_field(plist, field)
                .ok_or_else(|| format!("{service} plist 缺少 {field}"))?;
            let path = PathBuf::from(raw);
            if !path.is_absolute() {
                return Err(format!("{service} {field} 必须是绝对路径"));
            }
            let parent = path
                .parent()
                .ok_or_else(|| format!("{service} {field} 缺少父目录"))?;
            let canonical_parent = fs::canonicalize(parent)
                .map_err(|_| format!("{service} {field} 的父目录不存在或不可规范化"))?;
            let normalized = match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(format!("{service} {field} 不能是符号链接"));
                }
                Ok(metadata) if !metadata.file_type().is_file() => {
                    return Err(format!("{service} {field} 必须指向普通文件"));
                }
                Ok(_) => {
                    fs::canonicalize(&path).map_err(|_| format!("{service} {field} 不可规范化"))?
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    canonical_parent.join(path.file_name().unwrap_or_default())
                }
                Err(_) => return Err(format!("{service} {field} 不可读取")),
            };
            if paths.contains(&normalized) {
                return Err("Node 与 RInBot 的标准输出/错误日志路径必须互不相同".into());
            }
            paths.push(normalized);
        }
    }
    Ok(format!(
        "Node 控制台端口 {node_port} 与 RInBot 3211 不冲突，四条日志路径互不相同"
    ))
}

fn configured_argument<'a>(arguments: &'a [&'a str], name: &str) -> Option<&'a str> {
    arguments.iter().enumerate().find_map(|(index, argument)| {
        argument.strip_prefix(&format!("{name}=")).or_else(|| {
            (*argument == name)
                .then(|| arguments.get(index + 1).copied())
                .flatten()
        })
    })
}

fn reject_duplicate_rust_options(arguments: &[&str]) -> Result<(), String> {
    for option in ["--data-dir", "--ui-dir", "--host", "--port"] {
        let occurrences = arguments
            .iter()
            .filter(|argument| {
                **argument == option || argument.strip_prefix(&format!("{option}=")).is_some()
            })
            .count();
        if occurrences > 1 {
            return Err(format!("RInBot LaunchAgent 不得重复配置 {option}"));
        }
    }
    for flag in ["--active", "--connect-onebot"] {
        if arguments
            .iter()
            .any(|argument| argument.starts_with(&format!("{flag}=")))
        {
            return Err(format!(
                "RInBot LaunchAgent 的 {flag} 必须作为布尔开关单独配置"
            ));
        }
        if arguments
            .iter()
            .filter(|argument| **argument == flag)
            .count()
            > 1
        {
            return Err(format!("RInBot LaunchAgent 不得重复配置 {flag}"));
        }
    }
    Ok(())
}

fn inspect_rust_plist(path: &Path, rust_data_dir: &Path) -> Result<String, String> {
    let plist = read_launch_agent(path)?;
    if !lifecycle_and_log_paths_valid(&plist) {
        return Err(
            "RInBot plist 必须配置 RunAtLoad、异常退出 KeepAlive 及不同的绝对日志路径".into(),
        );
    }
    if string_field(&plist, "Label") != Some("com.local.rinbot") {
        return Err("RInBot LaunchAgent 的 Label 必须为 com.local.rinbot".into());
    }
    let arguments = string_array(&plist, "ProgramArguments")
        .filter(|arguments| !arguments.is_empty())
        .ok_or_else(|| "RInBot ProgramArguments 缺失或格式无效".to_owned())?;
    reject_duplicate_rust_options(&arguments)?;
    if !Path::new(arguments[0]).is_absolute() || !is_executable_file(Path::new(arguments[0])) {
        return Err("RInBot 启动程序必须是存在的绝对可执行文件".into());
    }
    let environment = dictionary(&plist, "EnvironmentVariables");
    let env_value = |key: &str| {
        environment
            .and_then(|values| values.get(key))
            .and_then(PlistValue::as_string)
    };
    let data_dir = configured_argument(&arguments, "--data-dir")
        .or_else(|| env_value("QQBOTD_DATA_DIR"))
        .ok_or_else(|| "RInBot 必须显式绑定 --data-dir 或 QQBOTD_DATA_DIR".to_owned())?;
    if !matching_existing_directory(data_dir, rust_data_dir) {
        return Err("RInBot 数据目录与指定隔离目录不一致".into());
    }
    let ui_dir = configured_argument(&arguments, "--ui-dir")
        .or_else(|| env_value("QQBOTD_UI_DIR"))
        .ok_or_else(|| "RInBot 必须显式绑定 --ui-dir 或 QQBOTD_UI_DIR".to_owned())?;
    if !valid_rust_ui_directory(ui_dir) {
        return Err("RInBot UI 目录必须是绝对路径，且包含有效的普通文件 index.html".into());
    }
    let active = arguments.contains(&"--active") || env_value("QQBOTD_ACTIVE") == Some("true");
    if !active {
        return Err("RInBot LaunchAgent 必须显式使用 active 运行模式".into());
    }
    let connect_onebot = arguments.contains(&"--connect-onebot")
        || env_value("QQBOTD_CONNECT_ONEBOT") == Some("true");
    if !connect_onebot {
        return Err("RInBot LaunchAgent 必须显式启用 NapCat/OneBot 连接".into());
    }
    if switch::rust_api_token_from_plist(path)?.is_none() {
        return Err(
            "RInBot 管理 API 必须配置 --api-token 或 QQBOTD_API_TOKEN，否则管理写操作不可用".into(),
        );
    }
    let host = configured_argument(&arguments, "--host")
        .or_else(|| env_value("QQBOTD_HOST"))
        .unwrap_or("127.0.0.1");
    let port = configured_argument(&arguments, "--port")
        .or_else(|| env_value("QQBOTD_PORT"))
        .unwrap_or("3211");
    if host != "127.0.0.1" || port != "3211" {
        return Err("RInBot API 必须仅监听 127.0.0.1:3211，与 Node 控制台隔离".into());
    }
    Ok("plist 已解析；Label、可执行程序、active/OneBot、隔离数据目录、UI 构建产物及本地 3211 端口均匹配（未执行服务操作）".into())
}

fn valid_rust_ui_directory(value: &str) -> bool {
    let configured = Path::new(value);
    if !configured.is_absolute() {
        return false;
    }
    let Ok(directory) = fs::canonicalize(configured) else {
        return false;
    };
    if !directory.is_dir() {
        return false;
    }
    let entry = directory.join("index.html");
    let Ok(metadata) = fs::symlink_metadata(&entry) else {
        return false;
    };
    metadata.file_type().is_file() && metadata.len() > 0 && metadata.len() <= 1024 * 1024
}

fn rust_ui_bundle_sha256(rust_plist: &Path) -> Result<String, String> {
    let plist = read_launch_agent(rust_plist)?;
    let arguments = string_array(&plist, "ProgramArguments")
        .filter(|arguments| !arguments.is_empty())
        .ok_or_else(|| "RInBot ProgramArguments 缺失或格式无效".to_owned())?;
    reject_duplicate_rust_options(&arguments)?;
    let environment = dictionary(&plist, "EnvironmentVariables");
    let ui_dir = configured_argument(&arguments, "--ui-dir")
        .or_else(|| {
            environment
                .and_then(|values| values.get("QQBOTD_UI_DIR"))
                .and_then(PlistValue::as_string)
        })
        .ok_or_else(|| "RInBot LaunchAgent 缺少显式 UI 目录".to_owned())?;
    if !valid_rust_ui_directory(ui_dir) {
        return Err("RInBot UI 目录或 index.html 无效".into());
    }
    ui_bundle_sha256(Path::new(ui_dir))
}

fn ui_bundle_sha256(directory: &Path) -> Result<String, String> {
    const MAX_FILES: usize = 10_000;
    const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
    const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;

    fn collect(
        root: &Path,
        directory: &Path,
        files: &mut Vec<(String, PathBuf)>,
        total_bytes: &mut u64,
    ) -> Result<(), String> {
        let mut entries = fs::read_dir(directory)
            .map_err(|_| "读取 RInBot UI 构建目录失败".to_owned())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "读取 RInBot UI 构建目录失败".to_owned())?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let metadata =
                fs::symlink_metadata(&path).map_err(|_| "读取 RInBot UI 构建项失败".to_owned())?;
            if metadata.file_type().is_dir() {
                collect(root, &path, files, total_bytes)?;
                continue;
            }
            if !metadata.file_type().is_file() {
                return Err("RInBot UI 构建目录不能包含符号链接或特殊文件".into());
            }
            if metadata.len() > MAX_FILE_BYTES {
                return Err("RInBot UI 单个构建文件超过 64 MiB".into());
            }
            *total_bytes = total_bytes
                .checked_add(metadata.len())
                .ok_or_else(|| "RInBot UI 构建目录大小溢出".to_owned())?;
            if *total_bytes > MAX_TOTAL_BYTES {
                return Err("RInBot UI 构建目录超过 128 MiB".into());
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| "RInBot UI 构建项越出目录".to_owned())?
                .to_str()
                .ok_or_else(|| "RInBot UI 构建路径必须为有效 UTF-8".to_owned())?
                .replace(std::path::MAIN_SEPARATOR, "/");
            files.push((relative, path));
            if files.len() > MAX_FILES {
                return Err("RInBot UI 构建目录文件数超过 10,000".into());
            }
        }
        Ok(())
    }

    let root = fs::canonicalize(directory).map_err(|_| "RInBot UI 目录不可规范化".to_owned())?;
    if !valid_rust_ui_directory(root.to_str().unwrap_or_default()) {
        return Err("RInBot UI 目录或 index.html 无效".into());
    }
    let mut files = Vec::new();
    let mut total_bytes = 0;
    collect(&root, &root, &mut files, &mut total_bytes)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256::new();
    hasher.update(b"RInBot static UI bundle v1\0");
    for (relative, path) in files {
        let digest = sha256_regular_file(&path, MAX_FILE_BYTES)?;
        hasher.update((relative.len() as u64).to_be_bytes());
        hasher.update(relative.as_bytes());
        hasher.update(digest.as_bytes());
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
fn verify_evidence(
    report_path: &Path,
    gate: &str,
    evidence: &GateEvidence,
    node_data_dir: &Path,
    rust_data_dir: &Path,
) -> Result<(), String> {
    verify_evidence_with_launch_agents(
        report_path,
        gate,
        evidence,
        node_data_dir,
        rust_data_dir,
        None,
    )
}

fn verify_evidence_with_launch_agents(
    report_path: &Path,
    gate: &str,
    evidence: &GateEvidence,
    node_data_dir: &Path,
    rust_data_dir: &Path,
    launch_agents: Option<(&Path, &Path)>,
) -> Result<(), String> {
    if !evidence.passed {
        return Err("验收报告未通过".into());
    }
    if evidence.sha256.len() != 64 || !evidence.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("证据 SHA-256 格式无效".into());
    }
    if evidence.evidence_file.is_absolute()
        || evidence
            .evidence_file
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err("证据路径必须位于验收报告目录内，不能使用绝对路径或上级目录".into());
    }
    let report_dir = report_path.parent().unwrap_or_else(|| Path::new("."));
    let evidence_path = report_dir.join(&evidence.evidence_file);
    let metadata =
        fs::symlink_metadata(&evidence_path).map_err(|_| "证据文件不存在或不可读".to_owned())?;
    if !metadata.file_type().is_file() {
        return Err("证据必须是普通文件，不能是目录或符号链接".into());
    }
    let canonical_report_dir =
        fs::canonicalize(report_dir).map_err(|_| "验收报告目录不可规范化".to_owned())?;
    let canonical_evidence =
        fs::canonicalize(&evidence_path).map_err(|_| "证据文件不可规范化".to_owned())?;
    if !canonical_evidence.starts_with(&canonical_report_dir) {
        return Err("证据文件解析后位于验收报告目录之外".into());
    }
    // Open once without following the final path component, then hash and
    // deserialize the exact same bounded byte buffer. Reopening the path for
    // JSON parsing would let a concurrent replacement swap validated evidence.
    const MAX_EVIDENCE_BYTES: u64 = 1024 * 1024;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(&evidence_path)
        .map_err(|_| "证据文件不可读或是符号链接".to_owned())?;
    let opened_metadata = file
        .metadata()
        .map_err(|_| "证据文件元数据不可读".to_owned())?;
    if !opened_metadata.is_file() {
        return Err("证据必须是普通文件，不能是目录或设备".into());
    }
    if opened_metadata.len() > MAX_EVIDENCE_BYTES {
        return Err("结构化验收证据不能超过 1 MiB".into());
    }
    let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
    file.take(MAX_EVIDENCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "读取证据文件失败".to_owned())?;
    if bytes.len() as u64 > MAX_EVIDENCE_BYTES {
        return Err("结构化验收证据不能超过 1 MiB".into());
    }
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if actual != evidence.sha256.to_ascii_lowercase() {
        return Err("证据文件 SHA-256 与报告不一致".into());
    }
    let document: GateEvidenceDocument =
        serde_json::from_slice(&bytes).map_err(|_| "证据必须是有效的结构化验收 JSON".to_owned())?;
    validate_gate_document(gate, &document)?;
    let rust_plist = launch_agents.map(|(_, rust)| rust);
    if gate_requires_rust_binary_binding(gate) {
        match rust_plist {
            Some(plist_path) => {
                let actual = rust_binary_sha256(plist_path)?;
                if document.rust_binary_sha256.as_deref() != Some(actual.as_str()) {
                    return Err(
                        "验收证据未绑定当前 RInBot LaunchAgent 可执行文件，或候选二进制已变化"
                            .into(),
                    );
                }
            }
            // Production preflight always provides the plist. The test-only
            // wrapper may omit it to exercise evidence structure independently.
            None if gate == "performance_acceptance" => {
                return Err("性能证据核验必须提供当前 RInBot LaunchAgent plist".into());
            }
            None => {}
        }
    }
    if gate == "launchd_configs_reviewed" {
        let (node_plist, rust_plist) =
            launch_agents.ok_or_else(|| "LaunchAgent 配置证据核验必须提供两份 plist".to_owned())?;
        if document.node_plist_sha256.as_deref()
            != Some(sha256_regular_file(node_plist, 1024 * 1024)?.as_str())
            || document.rust_plist_sha256.as_deref()
                != Some(sha256_regular_file(rust_plist, 1024 * 1024)?.as_str())
        {
            return Err("LaunchAgent 配置证据未绑定当前两份 plist，或启动配置已变化".into());
        }
        if document.node_config_sha256.as_deref()
            != Some(
                sha256_regular_file(&node_data_dir.join("config.json"), 4 * 1024 * 1024)?.as_str(),
            )
        {
            return Err("LaunchAgent 配置证据未绑定当前 Node 控制台配置".into());
        }
        let actual = rust_ui_bundle_sha256(rust_plist)?;
        if document.ui_bundle_sha256.as_deref() != Some(actual.as_str()) {
            return Err("LaunchAgent 配置证据未绑定当前 UI 构建目录，或静态资源已变化".into());
        }
    }
    if gate == "stability_24h" {
        let artifact_path = document
            .stability_samples_file
            .as_deref()
            .ok_or_else(|| "24 小时稳定性证据缺少原始样本文件路径".to_owned())?;
        let artifact_sha256 = document
            .stability_samples_sha256
            .as_deref()
            .ok_or_else(|| "24 小时稳定性证据缺少原始样本 SHA-256".to_owned())?;
        let artifact_bytes =
            read_bound_artifact(report_dir, artifact_path, artifact_sha256, 64 * 1024 * 1024)?;
        let artifact: serde_json::Value = serde_json::from_slice(&artifact_bytes)
            .map_err(|_| "24 小时稳定性原始样本不是有效 JSON".to_owned())?;
        validate_stability_sample_artifact(&document, &artifact)?;
    }
    if gate == "enabled_features_mapped" {
        let actual = sha256_regular_file(&node_data_dir.join("config.json"), 4 * 1024 * 1024)?;
        if document.node_config_sha256.as_deref() != Some(actual.as_str()) {
            return Err(
                "启用功能映射证据未绑定当前 Node config.json，或配置自证据生成后已变化".into(),
            );
        }
        let audit = rinbot::migration_audit::audit_directory(node_data_dir);
        if !audit.failures.is_empty() {
            return Err("当前 Node 数据目录迁移审计存在失败项".into());
        }
        let config = audit
            .config
            .ok_or_else(|| "当前 Node 迁移审计未能读取配置".to_owned())?;
        if document.enabled_features != config.enabled_features
            || document.unmapped_enabled_features != config.unmapped_enabled_features
            || !config.unmapped_enabled_features.is_empty()
        {
            return Err("启用功能证据清单与当前 Node 配置的迁移审计不一致".into());
        }
    }
    if gate == "unknown_outbox_reconciled" {
        let (actual, unresolved_count) = unknown_outbox_fingerprint(node_data_dir)?;
        if document.unknown_outbox_sha256.as_deref() != Some(actual.as_str()) {
            return Err(
                "未知外发核验证据未绑定当前 messages.sqlite unknown 账本，或账本已变化".into(),
            );
        }
        let (migration_report_sha256, _) = read_and_validate_migration_report(rust_data_dir)?;
        if document.migration_report_sha256.as_deref() != Some(migration_report_sha256.as_str()) {
            return Err("未知外发核验证据未绑定本次停写快照的 migration-report.json".into());
        }
        let (snapshot_fingerprint, snapshot_unresolved_count) =
            unknown_outbox_fingerprint_at(&rust_data_dir.join("legacy/messages.sqlite"))?;
        if actual != snapshot_fingerprint || unresolved_count != snapshot_unresolved_count {
            return Err("当前 Node unknown/sending 外发账本与停写快照归档不一致".into());
        }
        if unresolved_count > 0 {
            return Err(format!(
                "当前 messages.sqlite 仍有 {unresolved_count} 条 unknown/sending 外发未决，不能通过人工核验门槛"
            ));
        }
    }
    if gate == "node_snapshot_import_reconciled" {
        let (actual, report) = read_and_validate_migration_report(rust_data_dir)?;
        if document.migration_report_sha256.as_deref() != Some(actual.as_str()) {
            return Err("快照导入证据未绑定当前 RInBot 数据目录的 migration-report.json".into());
        }
        let current_config_sha256 =
            sha256_regular_file(&node_data_dir.join("config.json"), 4 * 1024 * 1024)?;
        let snapshot_config_sha256 =
            sha256_regular_file(&rust_data_dir.join("legacy/config.json"), 4 * 1024 * 1024)?;
        if current_config_sha256 != snapshot_config_sha256
            || document.node_config_sha256.as_deref() != Some(current_config_sha256.as_str())
        {
            return Err("当前 Node config.json 与停写快照归档配置不一致或证据已过期".into());
        }
        let proactive = report
            .get("proactive_state")
            .ok_or_else(|| "迁移报告缺少主动任务快照对账".to_owned())?;
        for (evidence_metric, report_metric) in [
            ("source_runtime_wake_count", "source_scheduled_wakes"),
            ("imported_runtime_wake_count", "imported_scheduled_wakes"),
            ("verified_runtime_wake_count", "verified_scheduled_wakes"),
            (
                "source_follow_up_cooldown_count",
                "source_follow_up_cooldowns",
            ),
            (
                "imported_follow_up_cooldown_count",
                "imported_follow_up_cooldowns",
            ),
            (
                "verified_follow_up_cooldown_count",
                "verified_follow_up_cooldowns",
            ),
        ] {
            if proactive
                .get(report_metric)
                .and_then(serde_json::Value::as_u64)
                != document
                    .metrics
                    .get(evidence_metric)
                    .and_then(|value| (*value >= 0.0).then_some(*value as u64))
            {
                return Err("快照导入报告与运行时唤醒计数不一致".into());
            }
        }
    }
    Ok(())
}

fn read_and_validate_migration_report(
    rust_data_dir: &Path,
) -> Result<(String, serde_json::Value), String> {
    let path = rust_data_dir.join("migration-report.json");
    let metadata = fs::symlink_metadata(&path)
        .map_err(|_| "RInBot 数据目录缺少 migration-report.json".to_owned())?;
    if !metadata.file_type().is_file() || metadata.len() > 8 * 1024 * 1024 {
        return Err("migration-report.json 必须是 8 MiB 以内的普通文件".into());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(&path)
        .map_err(|_| "migration-report.json 不可读或是符号链接".to_owned())?;
    let opened_metadata = file
        .metadata()
        .map_err(|_| "migration-report.json 元数据不可读".to_owned())?;
    if !opened_metadata.is_file() || opened_metadata.len() > 8 * 1024 * 1024 {
        return Err("migration-report.json 必须是 8 MiB 以内的普通文件".into());
    }
    let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
    file.take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "读取 migration-report.json 失败".to_owned())?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("migration-report.json 超过 8 MiB".into());
    }
    let report: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| "migration-report.json 不是有效 JSON".to_owned())?;
    let recorded_target_digest = report
        .get("target_data_sha256")
        .and_then(serde_json::Value::as_str)
        .filter(|digest| is_sha256_hex(digest))
        .ok_or_else(|| "migration-report.json 缺少有效的目标数据摘要".to_owned())?;
    let actual_target_digest =
        rinbot::migration_import::fingerprint_normalized_data_tree(rust_data_dir)
            .map_err(|_| "无法完整读取 RInBot 归一化运行数据树".to_owned())?;
    if recorded_target_digest != actual_target_digest {
        return Err("RInBot 归一化运行数据与迁移报告中的摘要不一致".into());
    }
    let recorded_legacy_digest = report
        .get("legacy_archive_sha256")
        .and_then(serde_json::Value::as_str)
        .filter(|digest| is_sha256_hex(digest))
        .ok_or_else(|| "migration-report.json 缺少有效的 Node 原档摘要".to_owned())?;
    let actual_legacy_digest =
        rinbot::migration_import::fingerprint_legacy_archive(&rust_data_dir.join("legacy"))
            .map_err(|_| "无法完整读取 RInBot 保留的 Node 原档".to_owned())?;
    if recorded_legacy_digest != actual_legacy_digest {
        return Err("RInBot 保留的 Node 原档与迁移报告中的摘要不一致".into());
    }
    let empty_array = |path: &[&str]| {
        path.iter()
            .try_fold(&report, |value, key| value.get(*key))
            .and_then(serde_json::Value::as_array)
            .is_some_and(Vec::is_empty)
    };
    if report.get("completed").and_then(serde_json::Value::as_bool) != Some(true)
        || report
            .get("target_integrity_ok")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        || report
            .get("copied_files")
            .and_then(serde_json::Value::as_u64)
            .is_none_or(|count| count == 0)
        || report
            .get("copied_bytes")
            .and_then(serde_json::Value::as_u64)
            .is_none_or(|count| count == 0)
        || !empty_array(&["source_audit", "failures"])
        || !empty_array(&["configuration", "unmapped_enabled_features"])
        || !empty_array(&["configuration", "enabled_pilot_data_missing"])
        || !empty_array(&["configuration", "enabled_feature_data_missing"])
    {
        return Err("migration-report.json 显示导入未完整、完整性失败或仍有阻断项".into());
    }
    let enabled = report
        .get("configuration")
        .and_then(|value| value.get("enabled_features"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "migration-report.json 缺少启用功能清单".to_owned())?;
    let proactive_enabled = enabled.iter().any(|feature| {
        feature
            .as_str()
            .is_some_and(|feature| matches!(feature, "proactive" | "followUp" | "selfWake"))
    });
    if proactive_enabled
        && report
            .get("proactive_state")
            .and_then(|value| value.get("runtime_snapshot_present"))
            .and_then(serde_json::Value::as_bool)
            != Some(true)
    {
        return Err("启用主动消息功能但 migration-report 未证明运行时任务快照".into());
    }
    Ok((format!("{:x}", Sha256::digest(&bytes)), report))
}

fn unknown_outbox_fingerprint(node_data_dir: &Path) -> Result<(String, usize), String> {
    unknown_outbox_fingerprint_at(&node_data_dir.join("messages.sqlite"))
}

type UnknownOutboxRow = (
    String,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
);

fn unknown_outbox_fingerprint_at(path: &Path) -> Result<(String, usize), String> {
    let metadata = fs::symlink_metadata(&path)
        .map_err(|_| "messages.sqlite 不存在或不可读，无法绑定未知外发证据".to_owned())?;
    if !metadata.file_type().is_file() {
        return Err("messages.sqlite 必须是普通文件，不能是符号链接或目录".into());
    }
    let connection = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "messages.sqlite 无法以只读方式打开".to_owned())?;
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|_| "messages.sqlite 只读查询初始化失败".to_owned())?;
    let mut statement = connection
        .prepare("SELECT id,run_id,chat_key,state,CASE WHEN length(CAST(payload AS BLOB))<=16777216 THEN payload ELSE NULL END,length(CAST(payload AS BLOB)),message_id,error FROM outbox WHERE state IN ('unknown','sending') ORDER BY id")
        .map_err(|_| "messages.sqlite 缺少可审计的 outbox unknown/sending 账本".to_owned())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })
        .map_err(|_| "读取 outbox unknown/sending 账本失败".to_owned())?;
    let mut unknown_rows = Vec::new();
    for row in rows {
        let (id, run_id, chat_key, state, payload, payload_bytes, message_id, error) =
            row.map_err(|_| "读取 outbox unknown/sending 账本失败".to_owned())?;
        if !(0..=16 * 1024 * 1024).contains(&payload_bytes) {
            return Err("未决外发 payload 超过 16 MiB 指纹上限".into());
        }
        let payload = payload.ok_or_else(|| "未决外发 payload 不可读取".to_owned())?;
        unknown_rows.push((id, run_id, chat_key, state, payload, message_id, error));
    }
    let unresolved_count = unknown_rows.len();
    Ok((
        unknown_outbox_fingerprint_from_rows(&unknown_rows),
        unresolved_count,
    ))
}

fn unknown_outbox_fingerprint_from_rows(rows: &[UnknownOutboxRow]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"RInBot unknown/sending outbox fingerprint v3\0");
    for (id, run_id, chat_key, state, payload, message_id, error) in rows {
        for value in [
            Some(id.as_str()),
            run_id.as_deref(),
            Some(chat_key.as_str()),
            Some(state.as_str()),
            Some(payload.as_str()),
            message_id.as_deref(),
            error.as_deref(),
        ] {
            match value {
                Some(value) => {
                    hasher.update([1]);
                    hasher.update((value.len() as u64).to_be_bytes());
                    hasher.update(value.as_bytes());
                }
                None => hasher.update([0]),
            }
        }
    }
    format!("{:x}", hasher.finalize())
}

fn sha256_regular_file(path: &Path, maximum_bytes: u64) -> Result<String, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "目标普通文件不存在或不可读".to_owned())?;
    if !metadata.file_type().is_file() || metadata.len() > maximum_bytes {
        return Err("目标必须是大小受限的普通文件".into());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|_| "目标文件不可读或是符号链接".to_owned())?;
    let opened_metadata = file
        .metadata()
        .map_err(|_| "目标文件元数据不可读".to_owned())?;
    if !opened_metadata.is_file() || opened_metadata.len() > maximum_bytes {
        return Err("目标必须是大小受限的普通文件".into());
    }
    let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
    file.take(maximum_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "读取目标文件失败".to_owned())?;
    if bytes.len() as u64 > maximum_bytes {
        return Err("目标文件超过允许大小".into());
    }
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

fn rust_binary_sha256(rust_plist: &Path) -> Result<String, String> {
    let plist = read_launch_agent(rust_plist)?;
    let arguments = string_array(&plist, "ProgramArguments")
        .filter(|arguments| !arguments.is_empty())
        .ok_or_else(|| "RInBot LaunchAgent ProgramArguments 缺失或格式无效".to_owned())?;
    let configured = Path::new(arguments[0]);
    let resolved = fs::canonicalize(configured)
        .map_err(|_| "RInBot LaunchAgent 可执行文件不可规范化".to_owned())?;
    if !is_executable_file(&resolved) {
        return Err("RInBot LaunchAgent 目标必须是可执行普通文件".into());
    }
    sha256_regular_file(&resolved, 512 * 1024 * 1024)
}

fn validate_gate_document(gate: &str, document: &GateEvidenceDocument) -> Result<(), String> {
    if document.format_version != 1 || document.gate != gate || !document.passed {
        return Err("证据版本、门槛名称、通过状态或记录时间无效".into());
    }
    const MAX_EVIDENCE_AGE_SECONDS: i64 = 7 * 24 * 60 * 60;
    const MAX_FUTURE_SKEW_SECONDS: i64 = 5 * 60;
    let recorded_at = time::OffsetDateTime::parse(
        &document.recorded_at,
        &time::format_description::well_known::Rfc3339,
    )
    .map_err(|_| "证据记录时间必须是有效 RFC3339 时间戳".to_owned())?;
    let age_seconds = (time::OffsetDateTime::now_utc() - recorded_at).whole_seconds();
    if age_seconds > MAX_EVIDENCE_AGE_SECONDS {
        return Err("验收证据超过 7 天有效期，必须重新核验".into());
    }
    if age_seconds < -MAX_FUTURE_SKEW_SECONDS {
        return Err("验收证据记录时间超出允许的未来时钟偏差".into());
    }
    let require_checks = |names: &[&str]| {
        names
            .iter()
            .all(|name| document.checks.get(*name) == Some(&true))
    };
    let metric = |name: &str| document.metrics.get(name).copied();
    let valid = match gate {
        "enabled_features_mapped" => {
            require_checks(&[
                "audit_matches_running_config",
                "all_enabled_features_mapped",
                "feature_behavior_matrix_complete",
                "enabled_data_requirements_met",
            ]) && document.unmapped_enabled_features.is_empty()
                && document.data_blockers.is_empty()
                && document
                    .node_config_sha256
                    .as_deref()
                    .is_some_and(is_sha256_hex)
        }
        "node_snapshot_import_reconciled" => {
            require_checks(&[
                "source_writer_stopped",
                "snapshot_integrity_ok",
                "import_integrity_ok",
                "counts_reconciled",
                "samples_reconciled",
                "rollback_copy_preserved",
                "runtime_wake_snapshot_captured_before_stop",
                "runtime_wakes_imported_without_dispatch",
                "runtime_wakes_reconciled",
            ]) && document.failed_records.is_empty()
                && nonnegative_integer_metric(document, "source_runtime_wake_count")
                    .zip(nonnegative_integer_metric(
                        document,
                        "imported_runtime_wake_count",
                    ))
                    .zip(nonnegative_integer_metric(
                        document,
                        "verified_runtime_wake_count",
                    ))
                    .is_some_and(|((source, imported), verified)| {
                        source == imported && imported == verified
                    })
                && nonnegative_integer_metric(document, "source_follow_up_cooldown_count")
                    .zip(nonnegative_integer_metric(
                        document,
                        "imported_follow_up_cooldown_count",
                    ))
                    .zip(nonnegative_integer_metric(
                        document,
                        "verified_follow_up_cooldown_count",
                    ))
                    .is_some_and(|((source, imported), verified)| {
                        source == imported && imported == verified
                    })
                && document
                    .migration_report_sha256
                    .as_deref()
                    .is_some_and(is_sha256_hex)
                && document
                    .node_config_sha256
                    .as_deref()
                    .is_some_and(is_sha256_hex)
        }
        "unknown_outbox_reconciled" => {
            require_checks(&[
                "every_unknown_manually_reviewed",
                "reconciliation_recorded",
                "no_automatic_retry_performed",
            ]) && metric("unknown_remaining_count") == Some(0.0)
                && document
                    .unknown_outbox_sha256
                    .as_deref()
                    .is_some_and(is_sha256_hex)
                && document
                    .migration_report_sha256
                    .as_deref()
                    .is_some_and(is_sha256_hex)
                && document.unresolved_items.is_empty()
        }
        "napcat_qq_acceptance" => require_checks(&[
            "real_napcat_connected",
            "private_and_group_inbound_verified",
            "text_image_voice_outbound_verified",
            "enabled_feature_cases_verified",
            "reconnect_and_duplicate_safety_verified",
            "external_write_outcomes_reconciled",
        ]),
        "performance_acceptance" => {
            require_checks(&[
                "bounded_queue_verified",
                "media_memory_bounded",
                "valid_image_fixture_verified",
                "same_host_and_dependencies",
            ]) && metric("idle_rss_mb").is_some_and(|value| value <= 40.0)
                && metric("typical_rss_p95_mb").is_some_and(|value| value <= 80.0)
                && metric("media_rss_peak_mb").is_some_and(|value| value <= 80.0)
                && metric("media_images_processed").is_some_and(|value| value >= 40.0)
                && metric("media_vision_requests")
                    .is_some_and(|value| metric("media_images_processed") == Some(value))
                && metric("valid_media_images_processed").is_some_and(|value| {
                    value >= 40.0 && metric("media_images_processed") == Some(value)
                })
                && metric("media_payload_max_mb")
                    .is_some_and(|value| (0.0..=7.0).contains(&value) && value > 0.0)
                && metric("media_sample_count").is_some_and(|value| value >= 180.0)
                && document
                    .media_fixture_sha256
                    .as_deref()
                    .is_some_and(is_sha256_hex)
        }
        "stability_24h" => {
            require_checks(&[
                "fixed_reproducible_load",
                "one_minute_samples",
                "api_status_healthy_each_sample",
                "queue_depth_stayed_within_capacity",
                "model_and_outbound_counts_reconciled",
                "no_sustained_post_warmup_rss_growth",
                "two_hour_warmup_excluded",
                "no_unreconciled_critical_errors",
            ]) && metric("observed_hours").is_some_and(|value| value >= 24.0)
                && metric("sample_count").is_some_and(|value| value >= 1_441.0)
                && metric("api_status_sample_count").is_some_and(|value| value >= 1_441.0)
                && metric("queue_depth_peak").is_some_and(|value| (0.0..=256.0).contains(&value))
                && metric("post_warmup_growth_mb").is_some_and(|value| value <= 10.0)
                && metric("warmup_seconds") == Some(7_200.0)
                && metric("sent_follow_up_wakes").is_some_and(|value| value >= 0.0)
                && metric("messages").is_some_and(|value| {
                    let follow_ups = metric("sent_follow_up_wakes").unwrap_or(-1.0);
                    let expected_total = value + follow_ups;
                    let vision_requests = metric("vision_model_requests").unwrap_or(-1.0);
                    metric("completed_inbound_batches") == Some(value)
                        && metric("primary_model_requests") == Some(expected_total)
                        && metric("model_requests") == Some(expected_total + vision_requests)
                        && metric("media_markers") == Some(2_097.0)
                        && vision_requests == 2_097.0
                        && metric("image_fetches") == Some(vision_requests)
                        && metric("valid_media_images_processed") == Some(vision_requests)
                        && metric("outbound_messages") == Some(expected_total)
                        && metric("sent_outbox") == Some(expected_total)
                })
                && document
                    .media_fixture_sha256
                    .as_deref()
                    .is_some_and(is_sha256_hex)
        }
        "ten_controlled_restarts" => {
            require_checks(&[
                "api_onebot_and_core_ready_each_round",
                "bad_candidate_automatically_rolled_back",
            ]) && metric("successful_rounds") == Some(10.0)
                && metric("max_recovery_seconds").is_some_and(|value| value <= 10.0)
        }
        "rollback_drill" => require_checks(&[
            "rust_candidate_stopped_before_node_restart",
            "node_version_and_snapshot_restored",
            "single_writer_invariant_preserved",
            "message_and_outbox_differences_reconciled",
            "rollback_health_verified",
        ]),
        // A Rust-to-Rust restart drill does not exercise the first migration
        // boundary. Require an isolated rehearsal of stopping the Node writer,
        // activating the imported Rust snapshot, and recovering Node after an
        // injected candidate failure before preflight can authorize cutover.
        "first_node_to_rust_cutover_drill" => require_checks(&[
            "isolated_launchd_environment_used",
            "node_writer_stopped_before_rust_start",
            "imported_snapshot_bound_to_rust_data_dir",
            "rust_api_onebot_and_core_ready_verified",
            "injected_rust_start_failure_restored_node",
            "single_writer_invariant_preserved",
            "message_and_outbox_differences_reconciled",
            "node_health_verified_after_recovery",
        ]),
        "launchd_configs_reviewed" => {
            require_checks(&[
                "node_label_program_and_data_binding_verified",
                "rust_label_program_and_data_binding_verified",
                "rust_ui_entrypoint_bound_and_present",
                "distinct_ports_and_logs_verified",
                "keepalive_and_single_writer_policy_verified",
                "offline_preflight_is_non_mutating",
            ]) && [
                document.node_plist_sha256.as_deref(),
                document.rust_plist_sha256.as_deref(),
                document.node_config_sha256.as_deref(),
            ]
            .iter()
            .all(|value| value.is_some_and(is_sha256_hex))
        }
        _ => return Err("验收报告包含未知门槛".into()),
    };
    if valid {
        if gate_requires_rust_binary_binding(gate)
            && !document
                .rust_binary_sha256
                .as_deref()
                .is_some_and(is_sha256_hex)
        {
            return Err("代码验收证据缺少有效的 RInBot 二进制 SHA-256".into());
        }
        Ok(())
    } else {
        Err("结构化证据缺少必需检查项，或测量值未达到门槛".into())
    }
}

fn gate_requires_rust_binary_binding(gate: &str) -> bool {
    matches!(
        gate,
        "enabled_features_mapped"
            | "node_snapshot_import_reconciled"
            | "napcat_qq_acceptance"
            | "performance_acceptance"
            | "stability_24h"
            | "ten_controlled_restarts"
            | "rollback_drill"
            | "first_node_to_rust_cutover_drill"
            | "launchd_configs_reviewed"
    )
}

fn read_bound_artifact(
    report_dir: &Path,
    relative_path: &Path,
    expected_sha256: &str,
    maximum_bytes: u64,
) -> Result<Vec<u8>, String> {
    if relative_path.as_os_str().is_empty()
        || relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err("原始证据文件路径必须位于验收报告目录内".into());
    }
    if !is_sha256_hex(expected_sha256) {
        return Err("原始证据文件 SHA-256 格式无效".into());
    }
    let artifact_path = report_dir.join(relative_path);
    let metadata = fs::symlink_metadata(&artifact_path)
        .map_err(|_| "原始证据文件不存在或不可读".to_owned())?;
    if !metadata.file_type().is_file() || metadata.len() > maximum_bytes {
        return Err("原始证据必须是大小受限的普通文件".into());
    }
    let canonical_report_dir =
        fs::canonicalize(report_dir).map_err(|_| "验收报告目录不可规范化".to_owned())?;
    let canonical_artifact =
        fs::canonicalize(&artifact_path).map_err(|_| "原始证据文件不可规范化".to_owned())?;
    if !canonical_artifact.starts_with(&canonical_report_dir) {
        return Err("原始证据文件解析后位于验收报告目录之外".into());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(&artifact_path)
        .map_err(|_| "原始证据文件不可读或是符号链接".to_owned())?;
    let opened_metadata = file
        .metadata()
        .map_err(|_| "原始证据文件元数据不可读".to_owned())?;
    if !opened_metadata.is_file() || opened_metadata.len() > maximum_bytes {
        return Err("原始证据必须是大小受限的普通文件".into());
    }
    let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
    file.take(maximum_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "读取原始证据文件失败".to_owned())?;
    if bytes.len() as u64 > maximum_bytes {
        return Err("原始证据文件超过允许大小".into());
    }
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if actual != expected_sha256.to_ascii_lowercase() {
        return Err("原始证据文件 SHA-256 与验收声明不一致".into());
    }
    Ok(bytes)
}

fn validate_stability_sample_artifact(
    evidence: &GateEvidenceDocument,
    artifact: &serde_json::Value,
) -> Result<(), String> {
    let summary = artifact
        .get("summary")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "稳定性原始证据缺少 summary".to_owned())?;
    let samples = artifact
        .get("samples")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "稳定性原始证据缺少 samples 数组".to_owned())?;
    if artifact
        .get("format_version")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return Err("稳定性原始证据 format_version 不受支持".into());
    }
    if samples.len() < 1_441
        || summary
            .get("sample_count")
            .and_then(serde_json::Value::as_u64)
            != Some(samples.len() as u64)
        || summary
            .get("api_status_sample_count")
            .and_then(serde_json::Value::as_u64)
            != Some(samples.len() as u64)
    {
        return Err("稳定性样本数与汇总不一致，或少于 1,441 点".into());
    }
    let mut rss_samples_kib = Vec::with_capacity(samples.len());
    let mut queue_depth_peak = 0_u64;
    let mut previous_timestamp = None;
    for (index, sample) in samples.iter().enumerate() {
        if sample
            .get("sample_index")
            .and_then(serde_json::Value::as_u64)
            != Some(index as u64)
            || sample
                .get("api_healthy")
                .and_then(serde_json::Value::as_bool)
                != Some(true)
            || sample
                .get("onebot_connected")
                .and_then(serde_json::Value::as_bool)
                != Some(true)
            || sample
                .get("core_ready")
                .and_then(serde_json::Value::as_bool)
                != Some(true)
        {
            return Err(format!(
                "第 {} 个稳定性样本未通过 API/OneBot/core 健康检查",
                index + 1
            ));
        }
        let timestamp = sample
            .get("sampled_at_unix_ms")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| format!("第 {} 个样本缺少有效时间戳", index + 1))?;
        if let Some(previous) = previous_timestamp {
            let gap = timestamp.saturating_sub(previous);
            if !(45_000..=90_000).contains(&gap) {
                return Err(format!("第 {} 个样本间隔超出 45–90 秒", index + 1));
            }
        }
        previous_timestamp = Some(timestamp);
        let rss = sample
            .get("rss_kib")
            .and_then(serde_json::Value::as_u64)
            .filter(|rss| *rss > 0)
            .ok_or_else(|| format!("第 {} 个样本缺少有效 RSS", index + 1))?;
        let queue_depth = sample
            .get("queue_depth")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| format!("第 {} 个样本缺少有效队列深度", index + 1))?;
        if queue_depth > 256 {
            return Err(format!("第 {} 个样本队列深度超过容量", index + 1));
        }
        rss_samples_kib.push(rss);
        queue_depth_peak = queue_depth_peak.max(queue_depth);
    }
    let first_timestamp = samples[0]["sampled_at_unix_ms"]
        .as_u64()
        .ok_or_else(|| "首个稳定性样本时间戳无效".to_owned())?;
    let last_timestamp = previous_timestamp.expect("samples is non-empty");
    let observed_seconds = last_timestamp.saturating_sub(first_timestamp) / 1_000;
    if observed_seconds < 86_400
        || summary
            .get("duration_seconds")
            .and_then(serde_json::Value::as_u64)
            != Some(observed_seconds)
    {
        return Err("稳定性样本首末时间未覆盖 24 小时，或汇总时长不匹配".into());
    }
    let near = |metric: &str, expected: f64, tolerance: f64| {
        evidence
            .metrics
            .get(metric)
            .is_some_and(|actual| actual.is_finite() && (actual - expected).abs() <= tolerance)
    };
    let summary_number = |name: &str| {
        summary
            .get(name)
            .and_then(serde_json::Value::as_f64)
            .filter(|value| value.is_finite())
    };
    let binary_sha = summary
        .get("rust_binary_sha256")
        .and_then(serde_json::Value::as_str);
    let expected_hours = observed_seconds as f64 / 3_600.0;
    if summary.get("messages").and_then(serde_json::Value::as_u64) != Some(8_064)
        || summary
            .get("media_markers")
            .and_then(serde_json::Value::as_u64)
            != Some(2_097)
        || summary
            .get("sample_interval_seconds")
            .and_then(serde_json::Value::as_u64)
            != Some(60)
        || binary_sha != evidence.rust_binary_sha256.as_deref()
        || !near("observed_hours", expected_hours, 1.0 / 3_600.0)
        || !near("sample_count", samples.len() as f64, 0.0)
        || !near("api_status_sample_count", samples.len() as f64, 0.0)
        || !near("queue_depth_peak", queue_depth_peak as f64, 0.0)
        || summary_number("queue_depth_peak") != Some(queue_depth_peak as f64)
    {
        return Err("稳定性验收指标与逐点原始样本或候选二进制哈希不一致".into());
    }
    for state_count in [
        "non_completed_inbound_batches",
        "non_sent_outbox",
        "failed_outbox",
        "unknown_outbox",
        "pending_outbox",
        "sending_outbox",
    ] {
        if summary.get(state_count).and_then(serde_json::Value::as_u64) != Some(0) {
            return Err(format!("稳定性账本存在未完成或非 sent 记录：{state_count}"));
        }
    }
    let metrics = [
        ("messages", "messages"),
        ("completed_inbound_batches", "completed_inbound_batches"),
        ("sent_follow_up_wakes", "sent_follow_up_wakes"),
        ("model_requests", "model_requests"),
        ("primary_model_requests", "primary_model_requests"),
        ("vision_model_requests", "vision_model_requests"),
        ("media_markers", "media_markers"),
        ("image_fetches", "image_fetches"),
        (
            "valid_media_images_processed",
            "valid_media_images_processed",
        ),
        ("outbound_messages", "outbound_messages"),
        ("sent_outbox", "sent_outbox"),
    ];
    for (evidence_key, summary_key) in metrics {
        let expected = summary
            .get(summary_key)
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| format!("稳定性汇总缺少 {summary_key}"))?;
        if !near(evidence_key, expected as f64, 0.0) {
            return Err(format!("稳定性 {evidence_key} 与原始账本汇总不一致"));
        }
    }
    if summary
        .get("media_fixture_kind")
        .and_then(serde_json::Value::as_str)
        != Some("valid-png")
        || summary
            .get("media_fixture_sha256")
            .and_then(serde_json::Value::as_str)
            != evidence.media_fixture_sha256.as_deref()
    {
        return Err("稳定性有效图片夹具类型或 SHA-256 与验收声明不一致".into());
    }
    // Format v1 reports contain a first/last-decile comparison which includes
    // startup. Validate that historical summary, then independently exclude
    // the RPD's first two hours for the acceptance metric.
    let growth_window = (rss_samples_kib.len() / 10).max(1);
    let mean_mb = |values: &[u64]| values.iter().sum::<u64>() as f64 / values.len() as f64 / 1024.0;
    let measured_growth = mean_mb(&rss_samples_kib[rss_samples_kib.len() - growth_window..])
        - mean_mb(&rss_samples_kib[..growth_window]);
    let summary_growth = summary_number("post_warmup_growth_mb")
        .ok_or_else(|| "稳定性汇总缺少暖机后 RSS 增长".to_owned())?;
    if (summary_growth - measured_growth).abs() > 0.02 {
        return Err("稳定性 RSS 增长汇总与逐点原始样本不一致".into());
    }
    let warmup_end = first_timestamp.saturating_add(7_200_000);
    let post_warmup_rss = samples
        .iter()
        .filter(|sample| sample["sampled_at_unix_ms"].as_u64().unwrap_or(0) >= warmup_end)
        .map(|sample| sample["rss_kib"].as_u64().expect("RSS already validated"))
        .collect::<Vec<_>>();
    if post_warmup_rss.len() < 2 {
        return Err("排除两小时预热后没有足够稳定性样本".into());
    }
    let post_warmup_window = (post_warmup_rss.len() / 10).max(1);
    let post_warmup_growth =
        mean_mb(&post_warmup_rss[post_warmup_rss.len() - post_warmup_window..])
            - mean_mb(&post_warmup_rss[..post_warmup_window]);
    if !near("post_warmup_growth_mb", post_warmup_growth, 0.02)
        || evidence.metrics.get("warmup_seconds") != Some(&7_200.0)
        || post_warmup_growth > 10.0
    {
        return Err("排除两小时预热后的 RSS 增长与原始样本不一致或超过门槛".into());
    }
    rss_samples_kib.sort_unstable();
    let p95 = rss_samples_kib[(rss_samples_kib.len() - 1) * 95 / 100] as f64 / 1024.0;
    let peak = *rss_samples_kib.last().expect("samples is non-empty") as f64 / 1024.0;
    if summary_number("rss_p95_mb").is_none_or(|reported| (reported - p95).abs() > 0.02)
        || summary_number("rss_peak_mb").is_none_or(|reported| (reported - peak).abs() > 0.02)
    {
        return Err("稳定性 RSS 分位数/峰值与逐点原始样本不一致".into());
    }
    Ok(())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn nonnegative_integer_metric(document: &GateEvidenceDocument, name: &str) -> Option<u64> {
    let value = document.metrics.get(name).copied()?;
    (value.is_finite() && value >= 0.0 && value.fract() == 0.0)
        .then_some(value as u64)
        .filter(|integer| *integer as f64 == value)
}

fn canonical_existing_dir(path: &Path) -> Option<PathBuf> {
    fs::canonicalize(path)
        .ok()
        .filter(|resolved| resolved.is_dir())
}

fn gate_label(gate: &str) -> &'static str {
    match gate {
        "enabled_features_mapped" => "启用功能迁移与等价验收",
        "node_snapshot_import_reconciled" => "Node 停写快照、导入与数据对账",
        "unknown_outbox_reconciled" => "未知发送结果人工核验",
        "napcat_qq_acceptance" => "真实 NapCat/QQ 验收",
        "performance_acceptance" => "性能验收",
        "stability_24h" => "24 小时稳定性观察",
        "ten_controlled_restarts" => "10 次受控重启/恢复测试",
        "rollback_drill" => "Node 回滚演练与差异对账",
        "first_node_to_rust_cutover_drill" => "首次 Node→Rust 切换与故障恢复演练",
        "launchd_configs_reviewed" => "launchd 配置审查",
        _ => "未知门槛",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn create_empty_messages_db(node_data_dir: &Path) {
        let connection = Connection::open(node_data_dir.join("messages.sqlite")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE outbox (
                    id TEXT PRIMARY KEY, run_id TEXT, chat_key TEXT NOT NULL,
                    state TEXT NOT NULL, payload TEXT NOT NULL, message_id TEXT, error TEXT
                );",
            )
            .unwrap();
    }

    fn write_report(path: &Path, value: serde_json::Value) {
        fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    }

    fn valid_gates(
        report_path: &Path,
        node_data_dir: &Path,
        rust_data_dir: &Path,
    ) -> serde_json::Value {
        let mut gates = serde_json::Map::new();
        for key in REQUIRED_GATES {
            let evidence_path = report_path
                .parent()
                .unwrap()
                .join(format!("{key}-evidence.json"));
            let mut document = valid_evidence_document(key);
            if *key == "enabled_features_mapped" {
                let audit = rinbot::migration_audit::audit_directory(node_data_dir);
                let config = audit
                    .config
                    .expect("feature-mapping fixture has a Node config audit");
                document["enabled_features"] = serde_json::json!(config.enabled_features);
                document["unmapped_enabled_features"] =
                    serde_json::json!(config.unmapped_enabled_features);
                document["node_config_sha256"] = serde_json::Value::String(
                    sha256_regular_file(&node_data_dir.join("config.json"), 4 * 1024 * 1024)
                        .expect("feature-mapping fixture config must be hashable"),
                );
            }
            if matches!(
                *key,
                "node_snapshot_import_reconciled" | "unknown_outbox_reconciled"
            ) {
                document["migration_report_sha256"] = serde_json::Value::String(
                    sha256_regular_file(
                        &rust_data_dir.join("migration-report.json"),
                        8 * 1024 * 1024,
                    )
                    .expect("valid migration report fixture must be hashable"),
                );
            }
            if *key == "node_snapshot_import_reconciled" {
                document["node_config_sha256"] = serde_json::Value::String(
                    sha256_regular_file(&node_data_dir.join("config.json"), 4 * 1024 * 1024)
                        .expect("snapshot fixture Node config must be hashable"),
                );
            }
            if *key == "launchd_configs_reviewed" {
                let ui_dir = report_path.parent().unwrap().join("rinbot-ui");
                document["ui_bundle_sha256"] = serde_json::Value::String(
                    ui_bundle_sha256(&ui_dir).expect("valid UI fixture must be hashable"),
                );
                for (field, path) in [
                    (
                        "node_plist_sha256",
                        report_path.parent().unwrap().join("node.plist"),
                    ),
                    (
                        "rust_plist_sha256",
                        report_path.parent().unwrap().join("rust.plist"),
                    ),
                    ("node_config_sha256", node_data_dir.join("config.json")),
                ] {
                    document[field] = sha256_regular_file(&path, 4 * 1024 * 1024).unwrap().into();
                }
            }
            if *key == "stability_24h" {
                let samples_path = report_path.parent().unwrap().join("stability-samples.json");
                let binary_sha256 = document["rust_binary_sha256"]
                    .as_str()
                    .expect("stability fixture must bind a binary SHA-256");
                let samples = valid_stability_samples(binary_sha256);
                fs::write(&samples_path, &samples).unwrap();
                document["stability_samples_file"] = serde_json::Value::String(
                    samples_path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
                document["stability_samples_sha256"] =
                    serde_json::Value::String(format!("{:x}", Sha256::digest(&samples)));
            }
            let evidence_bytes = serde_json::to_vec(&document).unwrap();
            fs::write(&evidence_path, &evidence_bytes).unwrap();
            gates.insert(
                (*key).to_string(),
                serde_json::json!({
                    "passed": true,
                    "evidence_file": evidence_path.file_name().unwrap().to_str().unwrap(),
                    "sha256": format!("{:x}", Sha256::digest(evidence_bytes)),
                }),
            );
        }
        serde_json::json!({"gates":gates})
    }

    fn valid_stability_samples(binary_sha256: &str) -> Vec<u8> {
        let samples = (0..=1_440)
            .map(|index| {
                serde_json::json!({
                    "sample_index":index,
                    "sampled_at_unix_ms":1_700_000_000_000_u64 + index * 60_000,
                    "rss_kib":20_480,
                    "queue_depth":1,
                    "api_healthy":true,
                    "onebot_connected":true,
                    "core_ready":true
                })
            })
            .collect::<Vec<_>>();
        serde_json::to_vec(&serde_json::json!({
            "format_version":1,
            "summary":{
                "sample_count":1441,
                "api_status_sample_count":1441,
                "duration_seconds":86400,
                "messages":8064,
                "media_markers":2097,
                "sample_interval_seconds":60,
                "completed_inbound_batches":8064,
                "non_completed_inbound_batches":0,
                "sent_follow_up_wakes":1,
                "model_requests":10162,
                "primary_model_requests":8065,
                "vision_model_requests":2097,
                "image_fetches":2097,
                "valid_media_images_processed":2097,
                "media_fixture_kind":"valid-png",
                "media_fixture_sha256":format!("{:x}", Sha256::digest(b"test valid PNG fixture")),
                "outbound_messages":8065,
                "sent_outbox":8065,
                "non_sent_outbox":0,
                "failed_outbox":0,
                "unknown_outbox":0,
                "pending_outbox":0,
                "sending_outbox":0,
                "queue_depth_peak":1,
                "post_warmup_growth_mb":0.0,
                "rss_p95_mb":20.0,
                "rss_peak_mb":20.0,
                "rust_binary_sha256":binary_sha256
            },
            "samples":samples
        }))
        .unwrap()
    }

    fn valid_migration_report_bytes() -> Vec<u8> {
        let empty = tempfile::tempdir().unwrap();
        let target_digest =
            rinbot::migration_import::fingerprint_normalized_data_tree(empty.path()).unwrap();
        let legacy = empty.path().join("legacy");
        fs::create_dir(&legacy).unwrap();
        fs::write(legacy.join("fixture.bin"), b"verified legacy fixture").unwrap();
        create_empty_messages_db(&legacy);
        let legacy_digest = rinbot::migration_import::fingerprint_legacy_archive(&legacy).unwrap();
        serde_json::to_vec(&serde_json::json!({
            "completed": true,
            "legacy_archive_sha256": legacy_digest,
            "copied_files": 1,
            "copied_bytes": 1,
            "target_integrity_ok": true,
            "target_data_sha256": target_digest,
            "source_audit": {"failures": []},
            "configuration": {
                "enabled_features": ["proactive"],
                "unmapped_enabled_features": [],
                "enabled_pilot_data_missing": [],
                "enabled_feature_data_missing": []
            },
            "proactive_state": {
                "runtime_snapshot_present": true,
                "source_scheduled_wakes": 0,
                "imported_scheduled_wakes": 0,
                "verified_scheduled_wakes": 0,
                "source_follow_up_cooldowns": 0,
                "imported_follow_up_cooldowns": 0,
                "verified_follow_up_cooldowns": 0
            }
        }))
        .unwrap()
    }

    fn valid_evidence_document(gate: &str) -> serde_json::Value {
        let checks: &[&str] = match gate {
            "enabled_features_mapped" => &[
                "audit_matches_running_config",
                "all_enabled_features_mapped",
                "feature_behavior_matrix_complete",
                "enabled_data_requirements_met",
            ],
            "node_snapshot_import_reconciled" => &[
                "source_writer_stopped",
                "snapshot_integrity_ok",
                "import_integrity_ok",
                "counts_reconciled",
                "samples_reconciled",
                "rollback_copy_preserved",
                "runtime_wake_snapshot_captured_before_stop",
                "runtime_wakes_imported_without_dispatch",
                "runtime_wakes_reconciled",
            ],
            "unknown_outbox_reconciled" => &[
                "every_unknown_manually_reviewed",
                "reconciliation_recorded",
                "no_automatic_retry_performed",
            ],
            "napcat_qq_acceptance" => &[
                "real_napcat_connected",
                "private_and_group_inbound_verified",
                "text_image_voice_outbound_verified",
                "enabled_feature_cases_verified",
                "reconnect_and_duplicate_safety_verified",
                "external_write_outcomes_reconciled",
            ],
            "performance_acceptance" => &[
                "bounded_queue_verified",
                "media_memory_bounded",
                "valid_image_fixture_verified",
                "same_host_and_dependencies",
            ],
            "stability_24h" => &[
                "fixed_reproducible_load",
                "one_minute_samples",
                "api_status_healthy_each_sample",
                "queue_depth_stayed_within_capacity",
                "model_and_outbound_counts_reconciled",
                "no_sustained_post_warmup_rss_growth",
                "two_hour_warmup_excluded",
                "no_unreconciled_critical_errors",
            ],
            "ten_controlled_restarts" => &[
                "api_onebot_and_core_ready_each_round",
                "bad_candidate_automatically_rolled_back",
            ],
            "rollback_drill" => &[
                "rust_candidate_stopped_before_node_restart",
                "node_version_and_snapshot_restored",
                "single_writer_invariant_preserved",
                "message_and_outbox_differences_reconciled",
                "rollback_health_verified",
            ],
            "first_node_to_rust_cutover_drill" => &[
                "isolated_launchd_environment_used",
                "node_writer_stopped_before_rust_start",
                "imported_snapshot_bound_to_rust_data_dir",
                "rust_api_onebot_and_core_ready_verified",
                "injected_rust_start_failure_restored_node",
                "single_writer_invariant_preserved",
                "message_and_outbox_differences_reconciled",
                "node_health_verified_after_recovery",
            ],
            "launchd_configs_reviewed" => &[
                "node_label_program_and_data_binding_verified",
                "rust_label_program_and_data_binding_verified",
                "rust_ui_entrypoint_bound_and_present",
                "distinct_ports_and_logs_verified",
                "keepalive_and_single_writer_policy_verified",
                "offline_preflight_is_non_mutating",
            ],
            _ => unreachable!(),
        };
        let checks = checks
            .iter()
            .map(|check| ((*check).to_owned(), serde_json::Value::Bool(true)))
            .collect::<serde_json::Map<_, _>>();
        let recorded_at = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        let mut document = serde_json::json!({
            "format_version": 1,
            "gate": gate,
            "passed": true,
            "recorded_at": recorded_at,
            "checks": checks,
            "metrics": {},
            "enabled_features": [],
            "unmapped_enabled_features": [],
            "data_blockers": [],
            "unresolved_items": [],
            "failed_records": [],
            "node_config_sha256": if matches!(
                gate,
                "enabled_features_mapped" | "node_snapshot_import_reconciled" | "launchd_configs_reviewed"
            ) {
                Some(format!(
                    "{:x}",
                    Sha256::digest(br#"{"server":{"host":"127.0.0.1","port":3210,"strictPort":true},"api":{"vision":false}}"#)
                ))
            } else {
                None::<String>
            },
            "unknown_outbox_sha256": if gate == "unknown_outbox_reconciled" {
                Some(unknown_outbox_fingerprint_from_rows(&[]))
            } else {
                None::<String>
            },
            "migration_report_sha256": if matches!(
                gate,
                "node_snapshot_import_reconciled" | "unknown_outbox_reconciled"
            ) {
                Some(format!("{:x}", Sha256::digest(valid_migration_report_bytes())))
            } else {
                None::<String>
            },
            "rust_binary_sha256": if gate_requires_rust_binary_binding(gate) {
                Some(format!("{:x}", Sha256::digest(b"test executable")))
            } else {
                None::<String>
            },
            "ui_bundle_sha256": None::<String>,
            "node_plist_sha256": if gate == "launchd_configs_reviewed" { Some("a".repeat(64)) } else { None },
            "rust_plist_sha256": if gate == "launchd_configs_reviewed" { Some("b".repeat(64)) } else { None },
            "stability_samples_file": if gate == "stability_24h" {
                Some("stability-samples.json")
            } else {
                None::<&str>
            },
            "stability_samples_sha256": if gate == "stability_24h" {
                Some(format!("{:x}", Sha256::digest(b"test stability samples")))
            } else {
                None::<String>
            },
            "media_fixture_sha256": if matches!(gate, "performance_acceptance" | "stability_24h") {
                Some(format!("{:x}", Sha256::digest(b"test valid PNG fixture")))
            } else {
                None::<String>
            },
        });
        match gate {
            "node_snapshot_import_reconciled" => {
                document["metrics"]["source_runtime_wake_count"] = 0.into();
                document["metrics"]["imported_runtime_wake_count"] = 0.into();
                document["metrics"]["verified_runtime_wake_count"] = 0.into();
                document["metrics"]["source_follow_up_cooldown_count"] = 0.into();
                document["metrics"]["imported_follow_up_cooldown_count"] = 0.into();
                document["metrics"]["verified_follow_up_cooldown_count"] = 0.into();
            }
            "unknown_outbox_reconciled" => {
                document["metrics"]["unknown_remaining_count"] = 0.into()
            }
            "performance_acceptance" => {
                document["metrics"]["idle_rss_mb"] = 30.into();
                document["metrics"]["typical_rss_p95_mb"] = 70.into();
                document["metrics"]["media_rss_peak_mb"] = 70.into();
                document["metrics"]["media_images_processed"] = 44.into();
                document["metrics"]["media_vision_requests"] = 44.into();
                document["metrics"]["valid_media_images_processed"] = 44.into();
                document["metrics"]["media_payload_max_mb"] = 6.into();
                document["metrics"]["media_sample_count"] = 180.into();
            }
            "stability_24h" => {
                document["metrics"]["observed_hours"] = 24.into();
                document["metrics"]["sample_count"] = 1441.into();
                document["metrics"]["api_status_sample_count"] = 1441.into();
                document["metrics"]["queue_depth_peak"] = 1.into();
                document["metrics"]["post_warmup_growth_mb"] = 0.into();
                document["metrics"]["warmup_seconds"] = 7200.into();
                document["metrics"]["messages"] = 8064.into();
                document["metrics"]["completed_inbound_batches"] = 8064.into();
                document["metrics"]["sent_follow_up_wakes"] = 1.into();
                document["metrics"]["model_requests"] = 10162.into();
                document["metrics"]["primary_model_requests"] = 8065.into();
                document["metrics"]["vision_model_requests"] = 2097.into();
                document["metrics"]["media_markers"] = 2097.into();
                document["metrics"]["image_fetches"] = 2097.into();
                document["metrics"]["valid_media_images_processed"] = 2097.into();
                document["metrics"]["outbound_messages"] = 8065.into();
                document["metrics"]["sent_outbox"] = 8065.into();
            }
            "ten_controlled_restarts" => {
                document["metrics"]["successful_rounds"] = 10.into();
                document["metrics"]["max_recovery_seconds"] = 9.5.into();
            }
            _ => {}
        }
        document
    }

    fn executable(path: &Path) {
        fs::write(path, b"test executable").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn plist(path: &Path, values: &[(&str, PlistValue)]) {
        let mut dictionary = plist::Dictionary::new();
        for (key, value) in values {
            dictionary.insert((*key).into(), value.clone());
        }
        PlistValue::Dictionary(dictionary)
            .to_file_xml(path)
            .unwrap();
    }

    fn arguments(values: &[&str]) -> PlistValue {
        PlistValue::Array(
            values
                .iter()
                .map(|value| PlistValue::String((*value).into()))
                .collect(),
        )
    }

    fn keep_alive() -> PlistValue {
        let mut dictionary = plist::Dictionary::new();
        dictionary.insert("SuccessfulExit".into(), PlistValue::Boolean(false));
        PlistValue::Dictionary(dictionary)
    }

    fn valid_launch_agents(temp: &Path, node: &Path, rust: &Path) -> (PathBuf, PathBuf) {
        let node_plist = temp.join("node.plist");
        let rust_plist = temp.join("rust.plist");
        let node_executable = temp.join("runtime/node");
        let rust_executable = temp.join("rinbot-runtime");
        let ui_dir = temp.join("rinbot-ui");
        let app = temp.join("app");
        fs::create_dir_all(app.join("src")).unwrap();
        fs::create_dir_all(&ui_dir).unwrap();
        fs::create_dir_all(ui_dir.join("assets")).unwrap();
        fs::create_dir_all(node_executable.parent().unwrap()).unwrap();
        fs::write(app.join("src/server.js"), "// fixture").unwrap();
        fs::write(ui_dir.join("index.html"), "<!doctype html><html></html>").unwrap();
        fs::write(ui_dir.join("assets/app.js"), "console.log('fixture');").unwrap();
        fs::create_dir(rust.join("legacy")).unwrap();
        fs::write(rust.join("legacy/fixture.bin"), b"verified legacy fixture").unwrap();
        create_empty_messages_db(&rust.join("legacy"));
        let mut migration_report: serde_json::Value =
            serde_json::from_slice(&valid_migration_report_bytes()).unwrap();
        migration_report["legacy_archive_sha256"] = serde_json::Value::String(
            rinbot::migration_import::fingerprint_legacy_archive(&rust.join("legacy")).unwrap(),
        );
        write_report(&rust.join("migration-report.json"), migration_report);
        executable(&node_executable);
        executable(&rust_executable);
        let mut node_environment = plist::Dictionary::new();
        fs::write(
            node.join("config.json"),
            br#"{"server":{"host":"127.0.0.1","port":3210,"strictPort":true},"api":{"vision":false}}"#,
        )
        .unwrap();
        fs::copy(node.join("config.json"), rust.join("legacy/config.json")).unwrap();
        let mut migration_report: serde_json::Value =
            serde_json::from_slice(&fs::read(rust.join("migration-report.json")).unwrap()).unwrap();
        migration_report["legacy_archive_sha256"] = serde_json::Value::String(
            rinbot::migration_import::fingerprint_legacy_archive(&rust.join("legacy")).unwrap(),
        );
        write_report(&rust.join("migration-report.json"), migration_report);
        node_environment.insert(
            "QQ_AGENT_DATA_DIR".into(),
            PlistValue::String(node.to_string_lossy().into_owned()),
        );
        plist(
            &node_plist,
            &[
                (
                    "Label",
                    PlistValue::String("com.local.qq-agent-plus".into()),
                ),
                (
                    "ProgramArguments",
                    arguments(&[node_executable.to_str().unwrap(), "src/server.js"]),
                ),
                (
                    "WorkingDirectory",
                    PlistValue::String(app.to_string_lossy().into_owned()),
                ),
                (
                    "EnvironmentVariables",
                    PlistValue::Dictionary(node_environment),
                ),
                ("RunAtLoad", PlistValue::Boolean(true)),
                ("KeepAlive", keep_alive()),
                (
                    "StandardOutPath",
                    PlistValue::String(temp.join("node.stdout.log").to_string_lossy().into_owned()),
                ),
                (
                    "StandardErrorPath",
                    PlistValue::String(temp.join("node.stderr.log").to_string_lossy().into_owned()),
                ),
            ],
        );
        plist(
            &rust_plist,
            &[
                ("Label", PlistValue::String("com.local.rinbot".into())),
                (
                    "ProgramArguments",
                    arguments(&[
                        rust_executable.to_str().unwrap(),
                        "--active",
                        "--connect-onebot",
                        "--data-dir",
                        rust.to_str().unwrap(),
                        "--ui-dir",
                        ui_dir.to_str().unwrap(),
                        "--host",
                        "127.0.0.1",
                        "--port",
                        "3211",
                        "--api-token",
                        "launchagent-fixture-token",
                    ]),
                ),
                ("RunAtLoad", PlistValue::Boolean(true)),
                ("KeepAlive", keep_alive()),
                (
                    "StandardOutPath",
                    PlistValue::String(
                        temp.join("rinbot.stdout.log")
                            .to_string_lossy()
                            .into_owned(),
                    ),
                ),
                (
                    "StandardErrorPath",
                    PlistValue::String(
                        temp.join("rinbot.stderr.log")
                            .to_string_lossy()
                            .into_owned(),
                    ),
                ),
            ],
        );
        (node_plist, rust_plist)
    }

    #[test]
    fn preflight_passes_only_with_separate_data_and_all_gates() {
        let temp = tempdir().unwrap();
        let node = temp.path().join("node-data");
        let rust = temp.path().join("rust-data");
        fs::create_dir(&node).unwrap();
        create_empty_messages_db(&node);
        fs::create_dir(&rust).unwrap();
        let (node_plist, rust_plist) = valid_launch_agents(temp.path(), &node, &rust);
        let report = temp.path().join("ready.json");
        write_report(&report, valid_gates(&report, &node, &rust));

        let checks = preflight(&node, &rust, &report, &node_plist, &rust_plist);
        assert!(checks.iter().all(|check| check.passed), "{checks:#?}");
    }

    #[test]
    fn rust_launch_agent_requires_an_explicit_existing_ui_bundle() {
        let temp = tempdir().unwrap();
        let node = temp.path().join("node-data");
        let rust = temp.path().join("rust-data");
        fs::create_dir(&node).unwrap();
        fs::create_dir(&rust).unwrap();
        let (_, rust_plist) = valid_launch_agents(temp.path(), &node, &rust);
        let mut launch_agent = PlistValue::from_file(&rust_plist).unwrap();
        let configured = string_array(&launch_agent, "ProgramArguments")
            .unwrap()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let mut without_ui_dir = Vec::new();
        let mut skip_next = false;
        for argument in configured {
            if skip_next {
                skip_next = false;
                continue;
            }
            if argument == "--ui-dir" {
                skip_next = true;
                continue;
            }
            without_ui_dir.push(argument);
        }
        let without_ui_dir = without_ui_dir
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        launch_agent
            .as_dictionary_mut()
            .unwrap()
            .insert("ProgramArguments".into(), arguments(&without_ui_dir));
        launch_agent.to_file_xml(&rust_plist).unwrap();
        assert!(inspect_rust_plist(&rust_plist, &rust)
            .unwrap_err()
            .contains("--ui-dir"));
    }

    #[test]
    fn rust_launch_agent_rejects_a_missing_ui_entrypoint() {
        let temp = tempdir().unwrap();
        let node = temp.path().join("node-data");
        let rust = temp.path().join("rust-data");
        fs::create_dir(&node).unwrap();
        fs::create_dir(&rust).unwrap();
        let (_, rust_plist) = valid_launch_agents(temp.path(), &node, &rust);
        let launch_agent = PlistValue::from_file(&rust_plist).unwrap();
        let configured = string_array(&launch_agent, "ProgramArguments").unwrap();
        let ui_dir = configured_argument(&configured, "--ui-dir").unwrap();
        fs::remove_file(Path::new(ui_dir).join("index.html")).unwrap();
        assert!(inspect_rust_plist(&rust_plist, &rust)
            .unwrap_err()
            .contains("index.html"));
    }

    #[test]
    fn rust_launch_agent_rejects_missing_administrator_authentication() {
        let temp = tempdir().unwrap();
        let node = temp.path().join("node-data");
        let rust = temp.path().join("rust-data");
        fs::create_dir(&node).unwrap();
        fs::create_dir(&rust).unwrap();
        let (_, rust_plist) = valid_launch_agents(temp.path(), &node, &rust);
        let mut launch_agent = PlistValue::from_file(&rust_plist).unwrap();
        let mut configured = string_array(&launch_agent, "ProgramArguments")
            .unwrap()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let index = configured
            .iter()
            .position(|value| value == "--api-token")
            .unwrap();
        configured.drain(index..=index + 1);
        let values = configured.iter().map(String::as_str).collect::<Vec<_>>();
        launch_agent
            .as_dictionary_mut()
            .unwrap()
            .insert("ProgramArguments".into(), arguments(&values));
        launch_agent.to_file_xml(&rust_plist).unwrap();
        assert!(inspect_rust_plist(&rust_plist, &rust)
            .unwrap_err()
            .contains("管理 API"));
    }

    #[test]
    fn rust_launch_agent_rejects_duplicate_runtime_paths() {
        let temp = tempdir().unwrap();
        let node = temp.path().join("node-data");
        let rust = temp.path().join("rust-data");
        fs::create_dir(&node).unwrap();
        fs::create_dir(&rust).unwrap();
        let (_, rust_plist) = valid_launch_agents(temp.path(), &node, &rust);
        let mut launch_agent = PlistValue::from_file(&rust_plist).unwrap();
        let mut configured = string_array(&launch_agent, "ProgramArguments")
            .unwrap()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let data_dir = rust.to_string_lossy().into_owned();
        configured.extend(["--data-dir".to_owned(), data_dir]);
        let configured = configured.iter().map(String::as_str).collect::<Vec<_>>();
        launch_agent
            .as_dictionary_mut()
            .unwrap()
            .insert("ProgramArguments".into(), arguments(&configured));
        launch_agent.to_file_xml(&rust_plist).unwrap();

        assert!(inspect_rust_plist(&rust_plist, &rust)
            .unwrap_err()
            .contains("不得重复配置 --data-dir"));
        assert!(rust_ui_bundle_sha256(&rust_plist)
            .unwrap_err()
            .contains("不得重复配置 --data-dir"));
    }

    #[test]
    fn rust_launch_agent_accepts_cli_override_of_environment_defaults() {
        let temp = tempdir().unwrap();
        let node = temp.path().join("node-data");
        let rust = temp.path().join("rust-data");
        fs::create_dir(&node).unwrap();
        fs::create_dir(&rust).unwrap();
        let (_, rust_plist) = valid_launch_agents(temp.path(), &node, &rust);
        let mut launch_agent = PlistValue::from_file(&rust_plist).unwrap();
        let mut environment = plist::Dictionary::new();
        environment.insert(
            "QQBOTD_DATA_DIR".into(),
            PlistValue::String(
                temp.path()
                    .join("ignored-env-data")
                    .to_string_lossy()
                    .into_owned(),
            ),
        );
        environment.insert(
            "QQBOTD_UI_DIR".into(),
            PlistValue::String(
                temp.path()
                    .join("ignored-env-ui")
                    .to_string_lossy()
                    .into_owned(),
            ),
        );
        launch_agent.as_dictionary_mut().unwrap().insert(
            "EnvironmentVariables".into(),
            PlistValue::Dictionary(environment),
        );
        launch_agent.to_file_xml(&rust_plist).unwrap();

        assert!(inspect_rust_plist(&rust_plist, &rust).is_ok());
    }

    #[test]
    fn rust_launch_agent_rejects_boolean_flags_with_inline_values() {
        assert!(reject_duplicate_rust_options(&["rinbot", "--active=false"])
            .unwrap_err()
            .contains("--active 必须作为布尔开关"));
        assert!(
            reject_duplicate_rust_options(&["rinbot", "--connect-onebot=true"])
                .unwrap_err()
                .contains("--connect-onebot 必须作为布尔开关")
        );
    }

    #[test]
    fn rust_ui_directory_rejects_relative_paths() {
        assert!(!valid_rust_ui_directory("ui/dist"));
    }

    #[test]
    fn launchd_review_evidence_is_invalidated_when_the_ui_bundle_changes() {
        let temp = tempdir().unwrap();
        let node = temp.path().join("node-data");
        let rust = temp.path().join("rust-data");
        fs::create_dir(&node).unwrap();
        create_empty_messages_db(&node);
        fs::create_dir(&rust).unwrap();
        let (node_plist, rust_plist) = valid_launch_agents(temp.path(), &node, &rust);
        let report = temp.path().join("ready.json");
        write_report(&report, valid_gates(&report, &node, &rust));

        let checks = preflight(&node, &rust, &report, &node_plist, &rust_plist);
        assert!(checks.iter().all(|check| check.passed), "{checks:#?}");

        fs::write(
            temp.path().join("rinbot-ui/assets/app.js"),
            "console.log('changed');",
        )
        .unwrap();
        let checks = preflight(&node, &rust, &report, &node_plist, &rust_plist);
        let launchd = checks
            .iter()
            .find(|check| check.name == "launchd 配置审查")
            .expect("LaunchAgent gate exists");
        assert!(!launchd.passed);
        assert!(launchd.detail.contains("UI 构建目录"));
    }

    #[test]
    fn offline_launchd_review_binds_exact_configs_without_exposing_credentials() {
        let temp = tempdir().unwrap();
        let node = temp.path().join("node");
        let rust = temp.path().join("rust");
        fs::create_dir(&node).unwrap();
        fs::create_dir(&rust).unwrap();
        let (node_plist, rust_plist) = valid_launch_agents(temp.path(), &node, &rust);
        let before_node = fs::read(&node_plist).unwrap();
        let before_rust = fs::read(&rust_plist).unwrap();
        let review = review_launchd(&node, &rust, &node_plist, &rust_plist).unwrap();
        let document: GateEvidenceDocument = serde_json::from_value(review.clone()).unwrap();
        validate_gate_document("launchd_configs_reviewed", &document).unwrap();
        assert_eq!(before_node, fs::read(&node_plist).unwrap());
        assert_eq!(before_rust, fs::read(&rust_plist).unwrap());
        assert!(!review.to_string().contains("launchagent-fixture-token"));
        assert!(!review.to_string().contains("QQBOTD_API_TOKEN"));
        assert!(review_launchd(&node, &node, &node_plist, &rust_plist).is_err());
        let report = temp.path().join("ready.json");
        let bytes = serde_json::to_vec(&review).unwrap();
        fs::write(temp.path().join("launchd.json"), &bytes).unwrap();
        let evidence = GateEvidence {
            passed: true,
            evidence_file: "launchd.json".into(),
            sha256: format!("{:x}", Sha256::digest(bytes)),
        };
        let verify = || {
            verify_evidence_with_launch_agents(
                &report,
                "launchd_configs_reviewed",
                &evidence,
                &node,
                &rust,
                Some((&node_plist, &rust_plist)),
            )
        };
        verify().unwrap();
        for path in [&node_plist, &rust_plist] {
            let original = fs::read(path).unwrap();
            fs::write(path, [original.as_slice(), b"\n"].concat()).unwrap();
            assert!(verify().unwrap_err().contains("启动配置已变化"));
            fs::write(path, original).unwrap();
        }
        let config = node.join("config.json");
        let original = fs::read(&config).unwrap();
        fs::write(&config, [original.as_slice(), b"\n"].concat()).unwrap();
        assert!(verify().unwrap_err().contains("Node 控制台配置"));
    }

    #[test]
    fn stability_gate_is_invalidated_when_raw_samples_change() {
        let temp = tempdir().unwrap();
        let node = temp.path().join("node-data");
        let rust = temp.path().join("rust-data");
        fs::create_dir(&node).unwrap();
        create_empty_messages_db(&node);
        fs::create_dir(&rust).unwrap();
        let (node_plist, rust_plist) = valid_launch_agents(temp.path(), &node, &rust);
        let report = temp.path().join("ready.json");
        write_report(&report, valid_gates(&report, &node, &rust));

        let checks = preflight(&node, &rust, &report, &node_plist, &rust_plist);
        assert!(checks.iter().all(|check| check.passed), "{checks:#?}");

        fs::write(temp.path().join("stability-samples.json"), b"{}").unwrap();
        let checks = preflight(&node, &rust, &report, &node_plist, &rust_plist);
        let stability = checks
            .iter()
            .find(|check| check.name == "24 小时稳定性观察")
            .expect("stability gate exists");
        assert!(!stability.passed);
        assert!(stability.detail.contains("SHA-256"));
    }

    #[test]
    fn stability_sample_validation_checks_health_spacing_and_fixed_load() {
        let document: GateEvidenceDocument =
            serde_json::from_value(valid_evidence_document("stability_24h")).unwrap();
        let bytes = valid_stability_samples(
            document
                .rust_binary_sha256
                .as_deref()
                .expect("fixture has a binary hash"),
        );
        let baseline: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        validate_stability_sample_artifact(&document, &baseline).unwrap();

        let mut unhealthy = baseline.clone();
        unhealthy["samples"][700]["onebot_connected"] = false.into();
        assert!(validate_stability_sample_artifact(&document, &unhealthy)
            .unwrap_err()
            .contains("健康检查"));

        let mut irregular = baseline.clone();
        irregular["samples"][700]["sampled_at_unix_ms"] = 1_700_000_000_000_u64
            .saturating_add(700 * 60_000)
            .saturating_add(200_000)
            .into();
        assert!(validate_stability_sample_artifact(&document, &irregular)
            .unwrap_err()
            .contains("间隔"));

        let mut underloaded = baseline;
        underloaded["summary"]["messages"] = 100.into();
        assert!(validate_stability_sample_artifact(&document, &underloaded).is_err());

        let mut unreconciled = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap();
        unreconciled["summary"]["unknown_outbox"] = 1.into();
        assert!(validate_stability_sample_artifact(&document, &unreconciled)
            .unwrap_err()
            .contains("unknown_outbox"));
    }

    #[test]
    fn stability_artifact_reader_rejects_traversal_symlinks_and_hash_drift() {
        let temp = tempdir().unwrap();
        let report_dir = temp.path().join("report");
        fs::create_dir(&report_dir).unwrap();
        let artifact = report_dir.join("samples.json");
        fs::write(&artifact, br#"{"samples":[]}"#).unwrap();
        let bytes = fs::read(&artifact).unwrap();
        let digest = format!("{:x}", Sha256::digest(&bytes));
        assert_eq!(
            read_bound_artifact(&report_dir, Path::new("samples.json"), &digest, 1024).unwrap(),
            bytes
        );
        assert!(
            read_bound_artifact(&report_dir, Path::new("../samples.json"), &digest, 1024)
                .unwrap_err()
                .contains("必须位于验收报告目录内")
        );
        assert!(read_bound_artifact(
            &report_dir,
            Path::new("samples.json"),
            &{
                let mut wrong = digest.clone();
                let replacement = if wrong.ends_with('0') { '1' } else { '0' };
                wrong.pop();
                wrong.push(replacement);
                wrong
            },
            1024
        )
        .unwrap_err()
        .contains("SHA-256 与验收声明不一致"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let link = report_dir.join("linked.json");
            symlink(&artifact, &link).unwrap();
            assert!(
                read_bound_artifact(&report_dir, Path::new("linked.json"), &digest, 1024).is_err()
            );
        }
    }

    #[test]
    fn stability_reconciles_vision_requests_and_rejects_missing_media() {
        let document: GateEvidenceDocument =
            serde_json::from_value(valid_evidence_document("stability_24h")).unwrap();
        validate_gate_document("stability_24h", &document).unwrap();
        let bytes = valid_stability_samples(document.rust_binary_sha256.as_deref().unwrap());
        let baseline: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        validate_stability_sample_artifact(&document, &baseline).unwrap();

        let mut omitted_vision = valid_evidence_document("stability_24h");
        omitted_vision["metrics"]["model_requests"] = 8065.into();
        assert!(validate_gate_document(
            "stability_24h",
            &serde_json::from_value(omitted_vision).unwrap()
        )
        .is_err());

        let mut incomplete_media = baseline.clone();
        incomplete_media["summary"]["valid_media_images_processed"] = 2096.into();
        assert!(validate_stability_sample_artifact(&document, &incomplete_media).is_err());

        let mut changed_fixture = baseline;
        changed_fixture["summary"]["media_fixture_sha256"] = "f".repeat(64).into();
        assert!(
            validate_stability_sample_artifact(&document, &changed_fixture)
                .unwrap_err()
                .contains("图片夹具")
        );
    }

    #[test]
    fn stability_excludes_two_hour_warmup_without_hiding_later_rss_growth() {
        let document: GateEvidenceDocument =
            serde_json::from_value(valid_evidence_document("stability_24h")).unwrap();
        let bytes = valid_stability_samples(document.rust_binary_sha256.as_deref().unwrap());
        let baseline: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        // 120 startup samples at 40 MiB, then 1,321 at 20 MiB. The old
        // first-decile mean is 36 2/3 MiB; after two hours growth is zero.
        let mut warmup_spike = baseline.clone();
        for sample in warmup_spike["samples"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .take(120)
        {
            sample["rss_kib"] = 40_960.into();
        }
        warmup_spike["summary"]["post_warmup_growth_mb"] = (-50.0 / 3.0).into();
        warmup_spike["summary"]["rss_p95_mb"] = 40.into();
        warmup_spike["summary"]["rss_peak_mb"] = 40.into();
        validate_stability_sample_artifact(&document, &warmup_spike).unwrap();

        // Startup is 80 MiB, post-warmup starts at 20 MiB and ends at 40 MiB.
        // Comparing startup with the end gives -30 MiB and would conceal
        // the actual +20 MiB growth after the required warmup exclusion.
        let mut hidden_growth = baseline;
        for (index, sample) in hidden_growth["samples"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            if index < 120 {
                sample["rss_kib"] = 81_920.into();
            } else if index >= 1_297 {
                sample["rss_kib"] = 40_960.into();
            }
        }
        hidden_growth["summary"]["post_warmup_growth_mb"] = (-30.0).into();
        hidden_growth["summary"]["rss_p95_mb"] = 80.into();
        hidden_growth["summary"]["rss_peak_mb"] = 80.into();
        assert!(
            validate_stability_sample_artifact(&document, &hidden_growth)
                .unwrap_err()
                .contains("排除两小时预热")
        );
    }

    #[test]
    fn preflight_fails_closed_on_missing_gate_and_overlapping_data() {
        let temp = tempdir().unwrap();
        let node = temp.path().join("node-data");
        fs::create_dir(&node).unwrap();
        create_empty_messages_db(&node);
        let rust = node.join("rust-data");
        fs::create_dir(&rust).unwrap();
        let (node_plist, rust_plist) = valid_launch_agents(temp.path(), &node, &rust);
        let report = temp.path().join("ready.json");
        let mut value = valid_gates(&report, &node, &rust);
        value["gates"]["rollback_drill"]["passed"] = serde_json::Value::Bool(false);
        write_report(&report, value);

        let checks = preflight(&node, &rust, &report, &node_plist, &rust_plist);
        assert!(checks
            .iter()
            .any(|check| check.name == "数据目录隔离" && !check.passed));
        assert!(checks
            .iter()
            .any(|check| check.name == "Node 回滚演练与差异对账" && !check.passed));
    }

    #[test]
    fn evidence_hash_mismatch_blocks_a_reported_pass() {
        let temp = tempdir().unwrap();
        let node = temp.path().join("node");
        let rust = temp.path().join("rust");
        fs::create_dir(&node).unwrap();
        create_empty_messages_db(&node);
        fs::create_dir(&rust).unwrap();
        let (node_plist, rust_plist) = valid_launch_agents(temp.path(), &node, &rust);
        let report = temp.path().join("ready.json");
        let value = valid_gates(&report, &node, &rust);
        fs::write(
            temp.path().join("enabled_features_mapped-evidence.json"),
            b"changed after review",
        )
        .unwrap();
        write_report(&report, value);

        let checks = preflight(&node, &rust, &report, &node_plist, &rust_plist);
        assert!(checks
            .iter()
            .any(|check| { check.name == "启用功能迁移与等价验收" && !check.passed }));
    }

    #[test]
    fn unknown_outbox_fingerprint_binds_payload_and_receipt_fields() {
        let original: Vec<UnknownOutboxRow> = vec![(
            "operation-1".into(),
            Some("run-1".into()),
            "group:123".into(),
            "unknown".into(),
            r#"{"type":"text","text":"original"}"#.into(),
            None,
            Some("response lost".into()),
        )];
        let mut changed_payload = original.clone();
        changed_payload[0].4 = r#"{"type":"text","text":"edited"}"#.into();
        assert_ne!(
            unknown_outbox_fingerprint_from_rows(&original),
            unknown_outbox_fingerprint_from_rows(&changed_payload)
        );

        let mut changed_receipt = original.clone();
        changed_receipt[0].5 = Some("message-99".into());
        assert_ne!(
            unknown_outbox_fingerprint_from_rows(&original),
            unknown_outbox_fingerprint_from_rows(&changed_receipt)
        );
    }

    #[test]
    fn unknown_outbox_evidence_is_bound_to_the_current_ledger() {
        let temp = tempdir().unwrap();
        let node = temp.path().join("node-data");
        let rust = temp.path().join("rust-data");
        fs::create_dir(&node).unwrap();
        fs::create_dir(&rust).unwrap();
        create_empty_messages_db(&node);
        fs::create_dir(rust.join("legacy")).unwrap();
        fs::write(rust.join("legacy/fixture.bin"), b"verified legacy fixture").unwrap();
        create_empty_messages_db(&rust.join("legacy"));
        let migration_report = rust.join("migration-report.json");
        let mut migration_report_value: serde_json::Value =
            serde_json::from_slice(&valid_migration_report_bytes()).unwrap();
        migration_report_value["legacy_archive_sha256"] = serde_json::Value::String(
            rinbot::migration_import::fingerprint_legacy_archive(&rust.join("legacy")).unwrap(),
        );
        write_report(&migration_report, migration_report_value);
        let report = temp.path().join("ready.json");
        let evidence_path = temp.path().join("unknown_outbox_reconciled-evidence.json");
        let mut document = valid_evidence_document("unknown_outbox_reconciled");
        document["migration_report_sha256"] = serde_json::Value::String(
            sha256_regular_file(&migration_report, 8 * 1024 * 1024).unwrap(),
        );
        let bytes = serde_json::to_vec(&document).unwrap();
        fs::write(&evidence_path, &bytes).unwrap();
        let evidence = GateEvidence {
            passed: true,
            evidence_file: evidence_path.file_name().unwrap().into(),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        };
        verify_evidence(
            &report,
            "unknown_outbox_reconciled",
            &evidence,
            &node,
            &rust,
        )
        .unwrap();

        let mut changed_report: serde_json::Value =
            serde_json::from_slice(&fs::read(&migration_report).unwrap()).unwrap();
        changed_report["review_revision"] = 2.into();
        write_report(&migration_report, changed_report);
        assert!(verify_evidence(
            &report,
            "unknown_outbox_reconciled",
            &evidence,
            &node,
            &rust
        )
        .unwrap_err()
        .contains("本次停写快照"));

        document["migration_report_sha256"] = serde_json::Value::String(
            sha256_regular_file(&migration_report, 8 * 1024 * 1024).unwrap(),
        );
        let refreshed_bytes = serde_json::to_vec(&document).unwrap();
        fs::write(&evidence_path, &refreshed_bytes).unwrap();
        let refreshed_evidence = GateEvidence {
            passed: true,
            evidence_file: evidence_path.file_name().unwrap().into(),
            sha256: format!("{:x}", Sha256::digest(&refreshed_bytes)),
        };
        verify_evidence(
            &report,
            "unknown_outbox_reconciled",
            &refreshed_evidence,
            &node,
            &rust,
        )
        .unwrap();

        let connection = Connection::open(node.join("messages.sqlite")).unwrap();
        connection
            .execute(
                "INSERT INTO outbox (id,run_id,chat_key,state,payload) VALUES (?1,?2,?3,'unknown','{}')",
                ("new-unknown", "run-1", "group:123"),
            )
            .unwrap();
        assert!(verify_evidence(
            &report,
            "unknown_outbox_reconciled",
            &refreshed_evidence,
            &node,
            &rust
        )
        .unwrap_err()
        .contains("账本已变化"));

        let snapshot_connection = Connection::open(rust.join("legacy/messages.sqlite")).unwrap();
        snapshot_connection
            .execute(
                "INSERT INTO outbox (id,run_id,chat_key,state,payload) VALUES (?1,?2,?3,'unknown','{}')",
                ("new-unknown", "run-1", "group:123"),
            )
            .unwrap();
        let mut migration_report_value: serde_json::Value =
            serde_json::from_slice(&fs::read(&migration_report).unwrap()).unwrap();
        migration_report_value["legacy_archive_sha256"] = serde_json::Value::String(
            rinbot::migration_import::fingerprint_legacy_archive(&rust.join("legacy")).unwrap(),
        );
        write_report(&migration_report, migration_report_value);
        document["unknown_outbox_sha256"] =
            serde_json::Value::String(unknown_outbox_fingerprint(&node).unwrap().0);
        document["migration_report_sha256"] = serde_json::Value::String(
            sha256_regular_file(&migration_report, 8 * 1024 * 1024).unwrap(),
        );
        let unresolved_bytes = serde_json::to_vec(&document).unwrap();
        fs::write(&evidence_path, &unresolved_bytes).unwrap();
        let unresolved_evidence = GateEvidence {
            passed: true,
            evidence_file: evidence_path.file_name().unwrap().into(),
            sha256: format!("{:x}", Sha256::digest(&unresolved_bytes)),
        };
        assert!(verify_evidence(
            &report,
            "unknown_outbox_reconciled",
            &unresolved_evidence,
            &node,
            &rust
        )
        .unwrap_err()
        .contains("仍有 1 条 unknown/sending"));

        connection
            .execute("DELETE FROM outbox WHERE id='new-unknown'", [])
            .unwrap();
        document["unknown_outbox_sha256"] =
            serde_json::Value::String(unknown_outbox_fingerprint(&node).unwrap().0);
        let reconciled_after_snapshot_bytes = serde_json::to_vec(&document).unwrap();
        fs::write(&evidence_path, &reconciled_after_snapshot_bytes).unwrap();
        let reconciled_after_snapshot_evidence = GateEvidence {
            passed: true,
            evidence_file: evidence_path.file_name().unwrap().into(),
            sha256: format!("{:x}", Sha256::digest(&reconciled_after_snapshot_bytes)),
        };
        assert!(verify_evidence(
            &report,
            "unknown_outbox_reconciled",
            &reconciled_after_snapshot_evidence,
            &node,
            &rust
        )
        .unwrap_err()
        .contains("与停写快照归档不一致"));

        connection
            .execute(
                "INSERT INTO outbox (id,run_id,chat_key,state,payload) VALUES (?1,?2,?3,'sending','{}')",
                ("new-unknown", "run-1", "group:123"),
            )
            .unwrap();
        snapshot_connection
            .execute(
                "UPDATE outbox SET state='sending' WHERE id='new-unknown'",
                [],
            )
            .unwrap();
        let mut migration_report_value: serde_json::Value =
            serde_json::from_slice(&fs::read(&migration_report).unwrap()).unwrap();
        migration_report_value["legacy_archive_sha256"] = serde_json::Value::String(
            rinbot::migration_import::fingerprint_legacy_archive(&rust.join("legacy")).unwrap(),
        );
        write_report(&migration_report, migration_report_value);
        document["unknown_outbox_sha256"] =
            serde_json::Value::String(unknown_outbox_fingerprint(&node).unwrap().0);
        document["migration_report_sha256"] = serde_json::Value::String(
            sha256_regular_file(&migration_report, 8 * 1024 * 1024).unwrap(),
        );
        let sending_bytes = serde_json::to_vec(&document).unwrap();
        fs::write(&evidence_path, &sending_bytes).unwrap();
        let sending_evidence = GateEvidence {
            passed: true,
            evidence_file: evidence_path.file_name().unwrap().into(),
            sha256: format!("{:x}", Sha256::digest(&sending_bytes)),
        };
        assert!(verify_evidence(
            &report,
            "unknown_outbox_reconciled",
            &sending_evidence,
            &node,
            &rust
        )
        .unwrap_err()
        .contains("仍有 1 条 unknown/sending"));
    }

    #[test]
    fn preflight_rejects_runtime_data_changed_after_import_report() {
        let temp = tempdir().unwrap();
        let node = temp.path().join("node-data");
        let rust = temp.path().join("rust-data");
        fs::create_dir(&node).unwrap();
        create_empty_messages_db(&node);
        fs::create_dir(&rust).unwrap();
        let (node_plist, rust_plist) = valid_launch_agents(temp.path(), &node, &rust);
        let report = temp.path().join("ready.json");
        write_report(&report, valid_gates(&report, &node, &rust));

        fs::write(rust.join("rinbot.sqlite"), b"changed after import").unwrap();
        let checks = preflight(&node, &rust, &report, &node_plist, &rust_plist);
        assert!(checks.iter().any(|check| {
            check.name == "Node 停写快照、导入与数据对账"
                && !check.passed
                && check.detail.contains("数据与迁移报告中的摘要不一致")
        }));
    }

    #[test]
    fn enabled_feature_evidence_is_bound_to_current_node_config() {
        let temp = tempdir().unwrap();
        let node = temp.path().join("node-data");
        fs::create_dir(&node).unwrap();
        let config = br#"{"server":{"host":"127.0.0.1","port":3210,"strictPort":true},"api":{"vision":false}}"#;
        fs::write(node.join("config.json"), config).unwrap();
        let report = temp.path().join("ready.json");
        let evidence_path = temp.path().join("enabled_features_mapped-evidence.json");
        let mut document = valid_evidence_document("enabled_features_mapped");
        let audit = rinbot::migration_audit::audit_directory(&node);
        let config_audit = audit.config.expect("fixture config audit exists");
        document["enabled_features"] = serde_json::json!(config_audit.enabled_features);
        document["unmapped_enabled_features"] =
            serde_json::json!(config_audit.unmapped_enabled_features);
        let bytes = serde_json::to_vec(&document).unwrap();
        fs::write(&evidence_path, &bytes).unwrap();
        let evidence = GateEvidence {
            passed: true,
            evidence_file: evidence_path.file_name().unwrap().into(),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        };
        verify_evidence(
            &report,
            "enabled_features_mapped",
            &evidence,
            &node,
            temp.path(),
        )
        .unwrap();

        document["enabled_features"] = serde_json::json!(["not-in-current-config"]);
        let changed_inventory_bytes = serde_json::to_vec(&document).unwrap();
        fs::write(&evidence_path, &changed_inventory_bytes).unwrap();
        let changed_inventory_evidence = GateEvidence {
            passed: true,
            evidence_file: evidence_path.file_name().unwrap().into(),
            sha256: format!("{:x}", Sha256::digest(&changed_inventory_bytes)),
        };
        assert!(verify_evidence(
            &report,
            "enabled_features_mapped",
            &changed_inventory_evidence,
            &node,
            temp.path(),
        )
        .unwrap_err()
        .contains("清单与当前 Node 配置"));

        fs::write(node.join("config.json"), b"{}\n").unwrap();
        assert!(verify_evidence(
            &report,
            "enabled_features_mapped",
            &changed_inventory_evidence,
            &node,
            temp.path(),
        )
        .unwrap_err()
        .contains("配置自证据生成后已变化"));
    }

    #[cfg(unix)]
    #[test]
    fn first_cutover_locked_recheck_rejects_config_changed_after_preview() {
        let temp = tempdir().unwrap();
        let node = temp.path().join("node-data");
        let rust = temp.path().join("rust-data");
        fs::create_dir(&node).unwrap();
        create_empty_messages_db(&node);
        fs::create_dir(&rust).unwrap();
        let (node_plist, rust_plist) = valid_launch_agents(temp.path(), &node, &rust);
        let report = temp.path().join("ready.json");
        write_report(&report, valid_gates(&report, &node, &rust));

        let release_root = temp.path().join("releases");
        fs::create_dir(&release_root).unwrap();
        let candidate_dir = release_root.join("candidate");
        fs::create_dir(&candidate_dir).unwrap();
        let candidate = candidate_dir.join("rinbot");
        executable(&candidate);
        let active_link = temp.path().join("active-rinbot");
        std::os::unix::fs::symlink(&candidate, &active_link).unwrap();

        let mut rust_agent = PlistValue::from_file(&rust_plist).unwrap();
        let arguments = rust_agent
            .as_dictionary_mut()
            .unwrap()
            .get_mut("ProgramArguments")
            .unwrap()
            .as_array_mut()
            .unwrap();
        arguments[0] = PlistValue::String(active_link.to_string_lossy().into_owned());
        rust_agent.to_file_xml(&rust_plist).unwrap();
        // Review the finalized active-link binding, not the fixture's earlier
        // executable path. Changing a plist must invalidate its old review.
        write_report(&report, valid_gates(&report, &node, &rust));

        assert!(first_cutover_static_blockers(
            &node,
            &rust,
            &report,
            &node_plist,
            &rust_plist,
            &active_link,
            &candidate,
            &release_root,
        )
        .is_empty());

        fs::write(
            node.join("config.json"),
            br#"{"server":{"host":"127.0.0.1","port":3210,"strictPort":true},"featureChanged":true}"#,
        )
        .unwrap();
        let blockers = first_cutover_static_blockers(
            &node,
            &rust,
            &report,
            &node_plist,
            &rust_plist,
            &active_link,
            &candidate,
            &release_root,
        );
        assert!(blockers
            .iter()
            .any(|blocker| { blocker.contains("配置自证据生成后已变化") }));
    }

    #[test]
    fn evidence_must_be_structured_and_match_its_gate() {
        let temp = tempdir().unwrap();
        let report = temp.path().join("ready.json");
        let evidence = temp.path().join("napcat_qq_acceptance-evidence.json");
        fs::write(&evidence, b"reviewed and passed").unwrap();
        let bytes = fs::read(&evidence).unwrap();
        let gate = GateEvidence {
            passed: true,
            evidence_file: evidence.file_name().unwrap().into(),
            sha256: format!("{:x}", Sha256::digest(bytes)),
        };
        assert!(verify_evidence(
            &report,
            "napcat_qq_acceptance",
            &gate,
            temp.path(),
            temp.path()
        )
        .is_err());

        let document = valid_evidence_document("performance_acceptance");
        fs::write(&evidence, serde_json::to_vec(&document).unwrap()).unwrap();
        let bytes = fs::read(&evidence).unwrap();
        let gate = GateEvidence {
            passed: true,
            evidence_file: evidence.file_name().unwrap().into(),
            sha256: format!("{:x}", Sha256::digest(bytes)),
        };
        assert!(verify_evidence(
            &report,
            "napcat_qq_acceptance",
            &gate,
            temp.path(),
            temp.path()
        )
        .is_err());
    }

    #[test]
    fn evidence_path_cannot_escape_readiness_report_directory() {
        let temp = tempdir().unwrap();
        let report_dir = temp.path().join("report");
        fs::create_dir(&report_dir).unwrap();
        let report = report_dir.join("ready.json");
        let outside = temp.path().join("evidence.json");
        fs::write(
            &outside,
            serde_json::to_vec(&valid_evidence_document("napcat_qq_acceptance")).unwrap(),
        )
        .unwrap();
        let gate = GateEvidence {
            passed: true,
            evidence_file: PathBuf::from("../evidence.json"),
            sha256: format!("{:x}", Sha256::digest(fs::read(&outside).unwrap())),
        };

        assert!(verify_evidence(
            &report,
            "napcat_qq_acceptance",
            &gate,
            temp.path(),
            temp.path()
        )
        .is_err());
    }

    #[test]
    fn oversized_gate_evidence_is_rejected_before_json_parsing() {
        let temp = tempdir().unwrap();
        let report = temp.path().join("ready.json");
        let evidence_path = temp.path().join("napcat_qq_acceptance-evidence.json");
        let bytes = vec![b' '; 1024 * 1024 + 1];
        fs::write(&evidence_path, &bytes).unwrap();
        let gate = GateEvidence {
            passed: true,
            evidence_file: evidence_path.file_name().unwrap().into(),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        };

        assert!(verify_evidence(
            &report,
            "napcat_qq_acceptance",
            &gate,
            temp.path(),
            temp.path(),
        )
        .unwrap_err()
        .contains("1 MiB"));
    }

    #[test]
    fn evidence_thresholds_block_unmapped_features_long_runs_and_slow_restarts() {
        let mut features = valid_evidence_document("enabled_features_mapped");
        features["unmapped_enabled_features"] = serde_json::json!(["relationshipPilot"]);
        assert!(validate_gate_document(
            "enabled_features_mapped",
            &serde_json::from_value(features).unwrap()
        )
        .is_err());

        let mut performance = valid_evidence_document("performance_acceptance");
        performance["metrics"]["media_images_processed"] = 0.into();
        assert!(validate_gate_document(
            "performance_acceptance",
            &serde_json::from_value(performance).unwrap()
        )
        .is_err());

        let mut performance = valid_evidence_document("performance_acceptance");
        performance["metrics"]["valid_media_images_processed"] = 39.into();
        assert!(validate_gate_document(
            "performance_acceptance",
            &serde_json::from_value(performance).unwrap()
        )
        .is_err());

        let mut performance = valid_evidence_document("performance_acceptance");
        performance["media_fixture_sha256"] = serde_json::Value::String("invalid".into());
        assert!(validate_gate_document(
            "performance_acceptance",
            &serde_json::from_value(performance).unwrap()
        )
        .is_err());

        let mut performance = valid_evidence_document("performance_acceptance");
        performance["metrics"]["media_rss_peak_mb"] = 80.1.into();
        assert!(validate_gate_document(
            "performance_acceptance",
            &serde_json::from_value(performance).unwrap()
        )
        .is_err());

        let mut stability = valid_evidence_document("stability_24h");
        stability["metrics"]["observed_hours"] = 23.into();
        assert!(validate_gate_document(
            "stability_24h",
            &serde_json::from_value(stability).unwrap()
        )
        .is_err());

        let mut stability = valid_evidence_document("stability_24h");
        stability["metrics"]["api_status_sample_count"] = 1439.into();
        assert!(validate_gate_document(
            "stability_24h",
            &serde_json::from_value(stability).unwrap()
        )
        .is_err());

        let mut stability = valid_evidence_document("stability_24h");
        stability["metrics"]["sample_count"] = 1440.into();
        stability["metrics"]["api_status_sample_count"] = 1440.into();
        assert!(validate_gate_document(
            "stability_24h",
            &serde_json::from_value(stability).unwrap()
        )
        .is_err());

        let mut stability = valid_evidence_document("stability_24h");
        stability["metrics"]["post_warmup_growth_mb"] = 10.1.into();
        assert!(validate_gate_document(
            "stability_24h",
            &serde_json::from_value(stability).unwrap()
        )
        .is_err());

        let mut stability = valid_evidence_document("stability_24h");
        stability["metrics"]["outbound_messages"] = 8066.into();
        assert!(validate_gate_document(
            "stability_24h",
            &serde_json::from_value(stability).unwrap()
        )
        .is_err());

        let mut restarts = valid_evidence_document("ten_controlled_restarts");
        restarts["metrics"]["max_recovery_seconds"] = 10.1.into();
        assert!(validate_gate_document(
            "ten_controlled_restarts",
            &serde_json::from_value(restarts).unwrap()
        )
        .is_err());

        // Do not let a generic Rust rollback drill stand in for the first
        // Node-to-Rust migration boundary and its Node recovery path.
        let mut first_cutover = valid_evidence_document("first_node_to_rust_cutover_drill");
        first_cutover["checks"]["injected_rust_start_failure_restored_node"] = false.into();
        assert!(validate_gate_document(
            "first_node_to_rust_cutover_drill",
            &serde_json::from_value(first_cutover).unwrap()
        )
        .is_err());
    }

    #[test]
    fn performance_evidence_is_bound_to_the_rust_launchagent_binary() {
        let temp = tempdir().unwrap();
        let node = temp.path().join("node-data");
        let rust = temp.path().join("rust-data");
        fs::create_dir(&node).unwrap();
        fs::create_dir(&rust).unwrap();
        let (node_plist, rust_plist) = valid_launch_agents(temp.path(), &node, &rust);
        let report = temp.path().join("ready.json");
        let evidence_path = temp.path().join("performance_acceptance-evidence.json");
        let document = valid_evidence_document("performance_acceptance");
        let bytes = serde_json::to_vec(&document).unwrap();
        fs::write(&evidence_path, &bytes).unwrap();
        let evidence = GateEvidence {
            passed: true,
            evidence_file: evidence_path.file_name().unwrap().into(),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        };

        verify_evidence_with_launch_agents(
            &report,
            "performance_acceptance",
            &evidence,
            &node,
            &rust,
            Some((&node_plist, &rust_plist)),
        )
        .unwrap();

        executable(&temp.path().join("rinbot-runtime"));
        fs::write(
            temp.path().join("rinbot-runtime"),
            b"different candidate binary",
        )
        .unwrap();
        assert!(verify_evidence_with_launch_agents(
            &report,
            "performance_acceptance",
            &evidence,
            &node,
            &rust,
            Some((&node_plist, &rust_plist)),
        )
        .unwrap_err()
        .contains("候选二进制已变化"));
    }

    #[test]
    fn evidence_documents_reject_stale_malformed_and_future_timestamps() {
        let mut document: GateEvidenceDocument =
            serde_json::from_value(valid_evidence_document("napcat_qq_acceptance")).unwrap();

        document.recorded_at = "2020-01-01T00:00:00Z".into();
        assert!(validate_gate_document("napcat_qq_acceptance", &document)
            .unwrap_err()
            .contains("超过 7 天"));

        document.recorded_at = "not-a-timestamp".into();
        assert!(validate_gate_document("napcat_qq_acceptance", &document)
            .unwrap_err()
            .contains("RFC3339"));

        document.recorded_at = "2999-01-01T00:00:00Z".into();
        assert!(validate_gate_document("napcat_qq_acceptance", &document)
            .unwrap_err()
            .contains("未来时钟偏差"));
    }

    #[test]
    fn snapshot_evidence_requires_exact_runtime_wake_reconciliation() {
        let mut document = valid_evidence_document("node_snapshot_import_reconciled");
        document["checks"]["runtime_wake_snapshot_captured_before_stop"] = false.into();
        assert!(validate_gate_document(
            "node_snapshot_import_reconciled",
            &serde_json::from_value(document.clone()).unwrap()
        )
        .is_err());

        document["checks"]["runtime_wake_snapshot_captured_before_stop"] = true.into();
        document["metrics"]["source_runtime_wake_count"] = 2.into();
        document["metrics"]["imported_runtime_wake_count"] = 1.into();
        document["metrics"]["verified_runtime_wake_count"] = 1.into();
        assert!(validate_gate_document(
            "node_snapshot_import_reconciled",
            &serde_json::from_value(document.clone()).unwrap()
        )
        .is_err());

        document["metrics"]["imported_runtime_wake_count"] = 2.into();
        document["metrics"]["verified_runtime_wake_count"] = 2.into();
        document["metrics"]["source_follow_up_cooldown_count"] = 1.into();
        assert!(validate_gate_document(
            "node_snapshot_import_reconciled",
            &serde_json::from_value(document.clone()).unwrap()
        )
        .is_err());
        document["metrics"]["imported_follow_up_cooldown_count"] = 1.into();
        document["metrics"]["verified_follow_up_cooldown_count"] = 1.into();
        assert!(validate_gate_document(
            "node_snapshot_import_reconciled",
            &serde_json::from_value(document).unwrap()
        )
        .is_ok());
    }

    #[test]
    fn snapshot_import_evidence_is_bound_to_the_current_rust_migration_report() {
        let temp = tempdir().unwrap();
        let node_config = temp.path().join("config.json");
        fs::write(
            &node_config,
            br#"{"server":{"host":"127.0.0.1","port":3210,"strictPort":true},"api":{"vision":false}}"#,
        )
        .unwrap();
        let rust_data = temp.path().join("rust-data");
        fs::create_dir(&rust_data).unwrap();
        fs::create_dir(rust_data.join("legacy")).unwrap();
        fs::write(
            rust_data.join("legacy/fixture.bin"),
            b"verified legacy fixture",
        )
        .unwrap();
        create_empty_messages_db(&rust_data.join("legacy"));
        fs::copy(&node_config, rust_data.join("legacy/config.json")).unwrap();
        let migration_report = rust_data.join("migration-report.json");
        let mut migration_report_value: serde_json::Value =
            serde_json::from_slice(&valid_migration_report_bytes()).unwrap();
        migration_report_value["legacy_archive_sha256"] = serde_json::Value::String(
            rinbot::migration_import::fingerprint_legacy_archive(&rust_data.join("legacy"))
                .unwrap(),
        );
        write_report(&migration_report, migration_report_value);
        let report = temp.path().join("ready.json");
        let evidence_path = temp
            .path()
            .join("node_snapshot_import_reconciled-evidence.json");
        let mut document = valid_evidence_document("node_snapshot_import_reconciled");
        document["migration_report_sha256"] = serde_json::Value::String(
            sha256_regular_file(&migration_report, 8 * 1024 * 1024).unwrap(),
        );
        let bytes = serde_json::to_vec(&document).unwrap();
        fs::write(&evidence_path, &bytes).unwrap();
        let evidence = GateEvidence {
            passed: true,
            evidence_file: evidence_path.file_name().unwrap().into(),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        };
        verify_evidence(
            &report,
            "node_snapshot_import_reconciled",
            &evidence,
            temp.path(),
            &rust_data,
        )
        .unwrap();

        fs::write(&node_config, br#"{"server":{"port":3210}}"#).unwrap();
        assert!(verify_evidence(
            &report,
            "node_snapshot_import_reconciled",
            &evidence,
            temp.path(),
            &rust_data,
        )
        .unwrap_err()
        .contains("当前 Node config.json 与停写快照归档配置不一致"));
        fs::write(
            &node_config,
            br#"{"server":{"host":"127.0.0.1","port":3210,"strictPort":true},"api":{"vision":false}}"#,
        )
        .unwrap();

        fs::write(
            rust_data.join("legacy/fixture.bin"),
            b"tampered legacy fixture",
        )
        .unwrap();
        assert!(verify_evidence(
            &report,
            "node_snapshot_import_reconciled",
            &evidence,
            temp.path(),
            &rust_data,
        )
        .unwrap_err()
        .contains("Node 原档与迁移报告中的摘要不一致"));
        fs::write(
            rust_data.join("legacy/fixture.bin"),
            b"verified legacy fixture",
        )
        .unwrap();

        let changed_report = fs::read(&migration_report).unwrap();
        fs::write(
            &migration_report,
            changed_report.into_iter().chain([b' ']).collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(verify_evidence(
            &report,
            "node_snapshot_import_reconciled",
            &evidence,
            temp.path(),
            &rust_data,
        )
        .unwrap_err()
        .contains("未绑定当前 RInBot 数据目录"));
    }

    #[test]
    fn malformed_report_is_a_blocker() {
        let temp = tempdir().unwrap();
        let node = temp.path().join("node");
        let rust = temp.path().join("rust");
        fs::create_dir(&node).unwrap();
        fs::create_dir(&rust).unwrap();
        let (node_plist, rust_plist) = valid_launch_agents(temp.path(), &node, &rust);
        let report = temp.path().join("ready.json");
        fs::write(&report, b"not json").unwrap();

        let checks = preflight(&node, &rust, &report, &node_plist, &rust_plist);
        assert!(checks
            .iter()
            .any(|check| check.name == "切换验收报告" && !check.passed));
    }

    #[test]
    fn launch_agent_preflight_rejects_wrong_rust_data_binding() {
        let temp = tempdir().unwrap();
        let node = temp.path().join("node-data");
        let rust = temp.path().join("rust-data");
        let wrong = temp.path().join("other-data");
        fs::create_dir(&node).unwrap();
        create_empty_messages_db(&node);
        fs::create_dir(&rust).unwrap();
        fs::create_dir(&wrong).unwrap();
        let (node_plist, rust_plist) = valid_launch_agents(temp.path(), &node, &rust);
        let mut launch_agent = PlistValue::from_file(&rust_plist).unwrap();
        let mut configured = string_array(&launch_agent, "ProgramArguments")
            .unwrap()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let data_dir_index = configured
            .iter()
            .position(|argument| argument == "--data-dir")
            .expect("Rust LaunchAgent data-dir argument exists");
        configured[data_dir_index + 1] = wrong.to_string_lossy().into_owned();
        let configured = configured.iter().map(String::as_str).collect::<Vec<_>>();
        launch_agent
            .as_dictionary_mut()
            .unwrap()
            .insert("ProgramArguments".into(), arguments(&configured));
        launch_agent.to_file_xml(&rust_plist).unwrap();
        let report = temp.path().join("ready.json");
        write_report(&report, valid_gates(&report, &node, &rust));

        let checks = preflight(&node, &rust, &report, &node_plist, &rust_plist);
        assert!(checks
            .iter()
            .any(|check| check.name == "RInBot LaunchAgent" && !check.passed));
    }

    #[test]
    fn node_readiness_binding_rejects_nonconflicting_but_wrong_listener() {
        let temp = tempdir().unwrap();
        for (host, port, strict) in [
            ("127.0.0.1", 3299, true),
            ("localhost", 3210, true),
            ("::1", 3210, true),
            ("127.0.0.1", 3299, false),
        ] {
            write_report(
                &temp.path().join("config.json"),
                serde_json::json!({
                    "server": {"host": host, "port": port, "strictPort": strict}
                }),
            );
            assert!(inspect_node_console_port(temp.path())
                .unwrap_err()
                .contains("停止确认和回滚健康检查地址不匹配"));
        }
    }

    #[test]
    fn preflight_rejects_node_port_fallback_overlap_and_cross_service_log_collision() {
        let temp = tempdir().unwrap();
        let node = temp.path().join("node-data");
        let rust = temp.path().join("rust-data");
        fs::create_dir(&node).unwrap();
        create_empty_messages_db(&node);
        fs::create_dir(&rust).unwrap();
        let (node_plist, rust_plist) = valid_launch_agents(temp.path(), &node, &rust);
        let report = temp.path().join("ready.json");
        write_report(&report, valid_gates(&report, &node, &rust));

        fs::write(
            node.join("config.json"),
            br#"{"server":{"host":"127.0.0.1","port":3208,"strictPort":false}}"#,
        )
        .unwrap();
        let checks = preflight(&node, &rust, &report, &node_plist, &rust_plist);
        assert!(checks.iter().any(|check| {
            check.name == "服务端口与日志隔离" && !check.passed && check.detail.contains("回退范围")
        }));

        fs::write(
            node.join("config.json"),
            br#"{"server":{"host":"127.0.0.1","port":3210,"strictPort":true}}"#,
        )
        .unwrap();
        let mut rust = PlistValue::from_file(&rust_plist).unwrap();
        let dictionary = rust.as_dictionary_mut().unwrap();
        dictionary.insert(
            "StandardOutPath".into(),
            PlistValue::String(temp.path().join("node.stdout.log").to_string_lossy().into()),
        );
        rust.to_file_xml(&rust_plist).unwrap();
        let checks = preflight(&node, &node, &report, &node_plist, &rust_plist);
        assert!(checks.iter().any(|check| {
            check.name == "服务端口与日志隔离" && !check.passed && check.detail.contains("日志路径")
        }));
    }
}
