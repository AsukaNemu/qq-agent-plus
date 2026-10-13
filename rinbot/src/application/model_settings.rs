//! Runtime model settings and private, host-bound API-key storage.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use reqwest::Url;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use crate::ports::language_model::ModelRuntimeConfig as ModelRuntimeSettings;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSettingsPublic {
    pub base_url: String,
    pub model: String,
    pub system_prompt: String,
    pub temperature: f32,
    pub max_tokens: u32,
    #[serde(default = "default_max_tool_rounds")]
    pub max_tool_rounds: u32,
    pub timeout_ms: u64,
    #[serde(default)]
    pub fallback: FallbackModelSettings,
    #[serde(default)]
    pub vision_model: VisionModelSettings,
}

fn default_max_tool_rounds() -> u32 {
    12
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VisionModelSettings {
    pub enabled: bool,
    pub base_url: String,
    pub model: String,
    pub timeout_ms: u64,
}

impl VisionModelSettings {
    fn validate(&self) -> Result<(), &'static str> {
        if !self.enabled {
            return Ok(());
        }
        let url = Url::parse(self.base_url.trim()).map_err(|_| "视觉模型服务地址必须是有效 URL")?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("视觉模型地址仅支持不含账号、密码、查询参数的 HTTP(S) URL");
        }
        if self.model.trim().is_empty() || self.model.chars().count() > 200 {
            return Err("视觉模型名称必须为 1 至 200 个字符");
        }
        if !(1_000..=300_000).contains(&self.timeout_ms) {
            return Err("视觉模型超时必须在 1 到 300 秒之间");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FallbackModelSettings {
    pub enabled: bool,
    pub base_url: String,
    pub model: String,
    pub timeout_ms: u64,
}

impl Default for FallbackModelSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: String::new(),
            model: String::new(),
            timeout_ms: 90_000,
        }
    }
}

impl ModelSettingsPublic {
    pub fn validate(&self) -> Result<(), &'static str> {
        let url = Url::parse(self.base_url.trim()).map_err(|_| "模型服务地址必须是有效 URL")?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("模型服务地址仅支持不含账号、密码、查询参数的 HTTP(S) URL");
        }
        if self.model.trim().is_empty() || self.model.chars().count() > 200 {
            return Err("模型名称必须为 1 至 200 个字符");
        }
        if self.system_prompt.trim().is_empty() || self.system_prompt.len() > 64 * 1024 {
            return Err("系统提示词必须为非空且不超过 64 KiB");
        }
        if !self.temperature.is_finite() || !(0.0..=2.0).contains(&self.temperature) {
            return Err("temperature 必须在 0 到 2 之间");
        }
        if !(1..=32_768).contains(&self.max_tokens) {
            return Err("最大输出 token 数必须在 1 到 32768 之间");
        }
        if !(1..=64).contains(&self.max_tool_rounds) {
            return Err("单次运行工具轮数必须在 1 到 64 之间");
        }
        if !(1_000..=300_000).contains(&self.timeout_ms) {
            return Err("模型超时必须在 1 到 300 秒之间");
        }
        self.vision_model.validate()?;
        if self.fallback.enabled {
            let url = Url::parse(self.fallback.base_url.trim())
                .map_err(|_| "备用模型服务地址必须是有效 URL")?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err("备用模型服务地址仅支持不含账号、密码、查询参数的 HTTP(S) URL");
            }
            if self.fallback.model.trim().is_empty() || self.fallback.model.chars().count() > 200 {
                return Err("备用模型名称必须为 1 至 200 个字符");
            }
            if !(1_000..=300_000).contains(&self.fallback.timeout_ms) {
                return Err("备用模型超时必须在 1 到 300 秒之间");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSettingsUpdate {
    pub public: ModelSettingsPublic,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub clear_api_key: bool,
    #[serde(default)]
    pub fallback_api_key: Option<String>,
    #[serde(default)]
    pub clear_fallback_api_key: bool,
    #[serde(default)]
    pub vision_api_key: Option<String>,
    #[serde(default)]
    pub clear_vision_api_key: bool,
}

#[derive(Debug, Error)]
pub enum CredentialStoreError {
    #[error("model credential storage must be in a private, non-symlink data directory")]
    UnsafeDirectory,
    #[error("model credential file is not a private regular file")]
    UnsafeFile,
    #[error("model credential file is invalid or too large")]
    InvalidFile,
    #[error("model credential storage I/O failed")]
    Io(#[from] std::io::Error),
    #[error("model credential serialization failed")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Clone)]
pub struct ModelCredentialStore {
    path: PathBuf,
}

impl ModelCredentialStore {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            path: data_dir.as_ref().join("model-credentials.json"),
        }
    }

    pub fn new_fallback(data_dir: impl AsRef<Path>) -> Self {
        Self {
            path: data_dir.as_ref().join("model-fallback-credentials.json"),
        }
    }

    pub fn new_vision(data_dir: impl AsRef<Path>) -> Self {
        Self {
            path: data_dir.as_ref().join("model-vision-credentials.json"),
        }
    }

    pub fn new_asr(data_dir: impl AsRef<Path>) -> Self {
        Self {
            path: data_dir.as_ref().join("asr-credentials.json"),
        }
    }

    pub fn key_for(&self, base_url: &str) -> Result<Option<String>, CredentialStoreError> {
        let host_key = endpoint_key(base_url).map_err(|_| CredentialStoreError::InvalidFile)?;
        Ok(self.read_all()?.remove(&host_key))
    }

    pub fn set_key_for(
        &self,
        base_url: &str,
        key: Option<&str>,
    ) -> Result<(), CredentialStoreError> {
        self.validate_parent()?;
        let host_key = endpoint_key(base_url).map_err(|_| CredentialStoreError::InvalidFile)?;
        let mut credentials = self.read_all()?;
        match key {
            Some(key) if !key.is_empty() => {
                if key.len() > 16 * 1024 {
                    return Err(CredentialStoreError::InvalidFile);
                }
                credentials.insert(host_key, key.to_owned());
            }
            _ => {
                // Keep a tombstone so clearing a key imported from the legacy
                // manifest cannot silently resurrect it after restart.
                credentials.insert(host_key, String::new());
            }
        }
        self.write_all(&credentials)
    }

    fn read_all(&self) -> Result<BTreeMap<String, String>, CredentialStoreError> {
        if !self.path.exists() {
            return Ok(BTreeMap::new());
        }
        self.validate_parent()?;
        let metadata =
            fs::symlink_metadata(&self.path).map_err(|_| CredentialStoreError::UnsafeFile)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 128 * 1024 {
            return Err(CredentialStoreError::UnsafeFile);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(CredentialStoreError::UnsafeFile);
            }
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        fs::File::open(&self.path)?
            .take(128 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 128 * 1024 {
            return Err(CredentialStoreError::InvalidFile);
        }
        serde_json::from_slice(&bytes).map_err(|_| CredentialStoreError::InvalidFile)
    }

    fn write_all(
        &self,
        credentials: &BTreeMap<String, String>,
    ) -> Result<(), CredentialStoreError> {
        let bytes = serde_json::to_vec(credentials)?;
        if bytes.len() > 128 * 1024 {
            return Err(CredentialStoreError::InvalidFile);
        }
        if let Ok(metadata) = fs::symlink_metadata(&self.path) {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(CredentialStoreError::UnsafeFile);
            }
        }
        let parent = self
            .path
            .parent()
            .ok_or(CredentialStoreError::UnsafeDirectory)?;
        let temporary = parent.join(format!(".model-credentials-{}.tmp", uuid::Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let write_result = (|| {
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
            }
            fs::rename(&temporary, &self.path)?;
            fs::File::open(parent)?.sync_all()?;
            Ok::<(), std::io::Error>(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write_result.map_err(CredentialStoreError::Io)
    }

    fn validate_parent(&self) -> Result<(), CredentialStoreError> {
        let parent = self
            .path
            .parent()
            .ok_or(CredentialStoreError::UnsafeDirectory)?;
        let metadata =
            fs::symlink_metadata(parent).map_err(|_| CredentialStoreError::UnsafeDirectory)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(CredentialStoreError::UnsafeDirectory);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(CredentialStoreError::UnsafeDirectory);
            }
        }
        Ok(())
    }
}

pub fn endpoint_key(base_url: &str) -> Result<String, ()> {
    let url = Url::parse(base_url.trim()).map_err(|_| ())?;
    Ok(format!(
        "{}://{}:{}",
        url.scheme().to_ascii_lowercase(),
        url.host_str().unwrap_or_default().to_ascii_lowercase(),
        url.port_or_known_default().unwrap_or_default()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[cfg(unix)]
    #[test]
    fn credentials_are_private_host_bound_and_never_debuggable() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let store = ModelCredentialStore::new(directory.path());
        store
            .set_key_for("https://model.example/v1", Some("private-key"))
            .unwrap();
        assert_eq!(
            store
                .key_for("https://model.example/v1")
                .unwrap()
                .as_deref(),
            Some("private-key")
        );
        assert_eq!(store.key_for("https://other.example/v1").unwrap(), None);
        let path = directory.path().join("model-credentials.json");
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(!format!(
            "{:?}",
            ModelRuntimeSettings {
                base_url: "https://model.example/v1".into(),
                model: "m".into(),
                api_key: "private-key".into(),
                system_prompt: "secret prompt".into(),
                temperature: 0.7,
                max_tokens: 100,
                max_tool_rounds: 12,
                timeout: Duration::from_secs(60),
                fallback: None,
                vision_model: None,
            }
        )
        .contains("private-key"));
    }

    #[test]
    fn model_settings_reject_urls_with_embedded_credentials_and_unbounded_values() {
        let settings = ModelSettingsPublic {
            base_url: "https://user:password@example.com/v1".into(),
            model: "model".into(),
            system_prompt: "prompt".into(),
            temperature: 0.7,
            max_tokens: 100,
            max_tool_rounds: 12,
            timeout_ms: 60_000,
            fallback: FallbackModelSettings::default(),
            vision_model: VisionModelSettings::default(),
        };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn enabled_fallback_requires_valid_endpoint_model_and_timeout() {
        let mut settings = ModelSettingsPublic {
            base_url: "https://primary.example/v1".into(),
            model: "primary".into(),
            system_prompt: "prompt".into(),
            temperature: 0.7,
            max_tokens: 512,
            max_tool_rounds: 12,
            timeout_ms: 90_000,
            fallback: FallbackModelSettings {
                enabled: true,
                base_url: "https://backup.example/v1".into(),
                model: "backup".into(),
                timeout_ms: 45_000,
            },
            vision_model: VisionModelSettings::default(),
        };
        assert_eq!(settings.validate(), Ok(()));

        settings.max_tool_rounds = 65;
        assert_eq!(
            settings.validate(),
            Err("单次运行工具轮数必须在 1 到 64 之间")
        );
        settings.max_tool_rounds = 12;

        settings.fallback.base_url = "ftp://backup.example/v1".into();
        assert_eq!(
            settings.validate(),
            Err("备用模型服务地址仅支持不含账号、密码、查询参数的 HTTP(S) URL")
        );
        settings.fallback.base_url = "https://backup.example/v1".into();
        settings.fallback.model.clear();
        assert_eq!(
            settings.validate(),
            Err("备用模型名称必须为 1 至 200 个字符")
        );
        settings.fallback.model = "backup".into();
        settings.fallback.timeout_ms = 0;
        assert_eq!(
            settings.validate(),
            Err("备用模型超时必须在 1 到 300 秒之间")
        );
    }
}
