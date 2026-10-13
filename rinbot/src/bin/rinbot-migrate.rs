use clap::Parser;
use rinbot::{
    migration_audit::{audit_directory, snapshot_directory},
    migration_import::import_stopped_node_directory,
};
use std::{
    fs,
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Parser)]
#[command(
    name = "rinbot-migrate",
    about = "Privacy-preserving audit and guarded snapshot tools for Node-to-RInBot migration"
)]
struct Args {
    #[command(subcommand)]
    action: Action,
}

#[derive(Debug, clap::Subcommand)]
enum Action {
    /// Read a directory without modifying it and print privacy-preserving counts.
    Audit {
        /// Existing Node data directory. The command opens SQLite databases read-only.
        #[arg(long)]
        source_dir: PathBuf,
    },
    /// Create and verify a local copy after stopping the Node writer.
    Snapshot {
        #[arg(long)]
        source_dir: PathBuf,
        #[arg(long)]
        destination_dir: PathBuf,
        /// Explicit operator assertion; snapshot is rejected without this flag.
        #[arg(long)]
        node_stopped: bool,
    },
    /// Build a new Rust data root with a verified legacy archive and normalized chat database.
    Import {
        #[arg(long)]
        source_dir: PathBuf,
        #[arg(long)]
        destination_dir: PathBuf,
        /// Explicit operator assertion; import is refused without this flag.
        #[arg(long)]
        node_stopped: bool,
    },
}

fn main() {
    let args = Args::parse();
    match args.action {
        Action::Audit { source_dir } => {
            if !source_dir.is_dir() {
                eprintln!("source directory does not exist or is not a directory");
                std::process::exit(2);
            }
            let report = audit_directory(source_dir);
            println!(
                "{}",
                serde_json::to_string_pretty(&report).expect("serialize report")
            );
            if !report.failures.is_empty() {
                std::process::exit(2);
            }
        }
        Action::Snapshot {
            source_dir,
            destination_dir,
            node_stopped,
        } => {
            if node_stopped {
                if let Err(error) = verify_node_writer_stopped(&source_dir) {
                    eprintln!("snapshot refused: {error}");
                    std::process::exit(2);
                }
            }
            match snapshot_directory(source_dir, destination_dir, node_stopped) {
                Ok(report) => println!(
                    "{}",
                    serde_json::to_string_pretty(&report).expect("serialize report")
                ),
                Err(error) => {
                    eprintln!("snapshot refused or failed: {error}");
                    std::process::exit(2);
                }
            }
        }
        Action::Import {
            source_dir,
            destination_dir,
            node_stopped,
        } => {
            if node_stopped {
                if let Err(error) = verify_node_writer_stopped(&source_dir) {
                    eprintln!("import refused: {error}");
                    std::process::exit(2);
                }
            }
            match import_stopped_node_directory(source_dir, destination_dir, node_stopped) {
                Ok(report) => println!(
                    "{}",
                    serde_json::to_string_pretty(&report).expect("serialize report")
                ),
                Err(error) => {
                    eprintln!("import refused or failed: {error}");
                    std::process::exit(2);
                }
            }
        }
    }
}

fn verify_node_writer_stopped(source_dir: &Path) -> Result<(), String> {
    let config_path = source_dir.join("config.json");
    let metadata = fs::symlink_metadata(&config_path)
        .map_err(|_| "无法安全读取 Node config.json".to_owned())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024 * 1024 {
        return Err("Node config.json 类型或大小不安全".into());
    }
    let config: serde_json::Value = serde_json::from_slice(
        &fs::read(&config_path).map_err(|_| "无法读取 Node config.json".to_owned())?,
    )
    .map_err(|_| "Node config.json 无法解析".to_owned())?;
    let server = config
        .get("server")
        .ok_or_else(|| "Node server 监听配置缺失".to_owned())?;
    if server.get("host").and_then(serde_json::Value::as_str) != Some("127.0.0.1")
        || server
            .get("strictPort")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        || server.get("port").and_then(serde_json::Value::as_u64) != Some(3210)
    {
        return Err("仅能验证固定监听于 127.0.0.1:3210 且 strictPort=true 的 Node 实例".into());
    }
    let address: SocketAddr = "127.0.0.1:3210".parse().expect("constant socket address");
    verify_listener_closed(address)?;

    #[cfg(target_os = "macos")]
    verify_node_launchagent_unloaded()?;
    Ok(())
}

fn verify_listener_closed(address: SocketAddr) -> Result<(), String> {
    match TcpStream::connect_timeout(&address, Duration::from_millis(300)) {
        Ok(_) => Err(format!("仍有进程监听 {}", address)),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => Ok(()),
        Err(_) => Err(format!("无法确认 {} 已停止监听", address)),
    }
}

#[cfg(target_os = "macos")]
fn verify_node_launchagent_unloaded() -> Result<(), String> {
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map_err(|_| "无法读取当前 macOS 用户 UID".to_owned())?;
    if !uid.status.success() {
        return Err("无法读取当前 macOS 用户 UID".into());
    }
    let uid = String::from_utf8(uid.stdout).map_err(|_| "当前用户 UID 无效".to_owned())?;
    let uid = uid.trim();
    if uid.is_empty() || !uid.bytes().all(|byte| byte.is_ascii_digit()) || uid == "0" {
        return Err("拒绝以 root 身份验证用户 LaunchAgent".into());
    }
    let target = format!("gui/{uid}/com.local.qq-agent-plus");
    let output = std::process::Command::new("launchctl")
        .args(["print", target.as_str()])
        .output()
        .map_err(|_| "无法只读查询 Node LaunchAgent".to_owned())?;
    match output.status.code() {
        None => Err("无法读取 Node LaunchAgent 查询结果".into()),
        Some(0) => Err("Node LaunchAgent 仍处于加载状态".into()),
        Some(113) => Ok(()),
        Some(_) => Err("Node LaunchAgent 状态不明确，拒绝迁移".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn stopped_writer_flag_cannot_bypass_strict_loopback_configuration_check() {
        let source = tempdir().unwrap();
        fs::write(
            source.path().join("config.json"),
            br#"{"server":{"host":"127.0.0.1","port":3210,"strictPort":false}}"#,
        )
        .unwrap();
        assert!(verify_node_writer_stopped(source.path())
            .unwrap_err()
            .contains("strictPort=true"));
    }

    #[test]
    fn stopped_writer_probe_rejects_a_live_listener_and_accepts_a_closed_port() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        assert!(verify_listener_closed(address).is_err());
        drop(listener);
        assert!(verify_listener_closed(address).is_ok());
    }
}
