//! Bounded audio normalization and OpenAI-compatible speech transcription.
//!
//! Provider credentials stay outside settings and are supplied by the runtime credential store.

use std::{
    ffi::{OsStr, OsString},
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::Mutex,
    time::Duration,
};

use reqwest::{multipart, Client, Url};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{io::AsyncReadExt, process::Command};

pub const MAX_AUDIO_BYTES: usize = 200 * 1024 * 1024;
pub const MAX_PCM_SECONDS: usize = 15 * 60;
pub const PCM_BYTES_PER_SECOND: usize = 32_000;
pub const MAX_PCM_BYTES: usize = MAX_PCM_SECONDS * PCM_BYTES_PER_SECOND;
pub const OPENAI_WAV_MAX_BYTES: usize = 20 * 1024 * 1024;

pub fn resolve_ffmpeg_path() -> OsString {
    if let Some(path) = std::env::var_os("FFMPEG").filter(|path| !path.is_empty()) {
        return path;
    }
    if let Ok(executable) = std::env::current_exe() {
        for ancestor in executable.ancestors() {
            let bundled = ancestor.join("runtime").join("bin").join(if cfg!(windows) {
                "ffmpeg.exe"
            } else {
                "ffmpeg"
            });
            if bundled.is_file() {
                return bundled.into_os_string();
            }
        }
    }
    OsString::from("ffmpeg")
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AsrSettings {
    pub enabled: bool,
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub language: String,
    pub max_per_hour: u32,
    pub timeout_ms: u64,
}

impl Default for AsrSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "openai".into(),
            base_url: String::new(),
            model: String::new(),
            language: String::new(),
            max_per_hour: 12,
            timeout_ms: 300_000,
        }
    }
}

impl AsrSettings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if ![
            "openai", "volc", "aliyun", "baidu", "tencent", "iflytek", "local",
        ]
        .contains(&self.provider.trim().to_ascii_lowercase().as_str())
        {
            return Err("不支持的语音识别服务标识");
        }
        if !(1..=200).contains(&self.max_per_hour) {
            return Err("每小时转写次数必须在 1 到 200 之间");
        }
        if !(1_000..=600_000).contains(&self.timeout_ms) {
            return Err("语音识别超时必须在 1 到 600 秒之间");
        }
        if self.language.len() > 40
            || self.language.chars().any(char::is_control)
            || self.model.len() > 200
        {
            return Err("语言或模型字段超出范围");
        }
        if self.provider.eq_ignore_ascii_case("openai") && !self.base_url.trim().is_empty() {
            let url = Url::parse(self.base_url.trim()).map_err(|_| "语音识别地址无效")?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err("语音识别地址只允许不含凭据、查询参数的 HTTP(S) URL");
            }
        }
        Ok(())
    }

    pub fn configured(&self, api_key: &str) -> bool {
        self.enabled
            && self.provider.eq_ignore_ascii_case("openai")
            && !api_key.trim().is_empty()
            && !self.base_url.trim().is_empty()
            && !self.model.trim().is_empty()
    }
}

#[derive(Clone)]
pub struct AsrRuntimeConfig {
    pub settings: AsrSettings,
    pub api_key: String,
}

impl std::fmt::Debug for AsrRuntimeConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AsrRuntimeConfig")
            .field("settings", &self.settings)
            .field(
                "api_key",
                &if self.api_key.is_empty() {
                    "[EMPTY]"
                } else {
                    "[REDACTED]"
                },
            )
            .finish()
    }
}

#[derive(Default)]
pub struct AsrQuota {
    state: Mutex<(i64, u32)>,
}

impl AsrQuota {
    /// Process-local billing guard, matching the existing Node quota semantics.
    pub fn consume(&self, now_ms: i64, limit: u32) -> bool {
        let hour = now_ms.div_euclid(3_600_000);
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.0 != hour {
            *state = (hour, 0);
        }
        if state.1 >= limit.clamp(1, 200) {
            return false;
        }
        state.1 += 1;
        true
    }
}

#[derive(Debug, Error)]
pub enum AsrError {
    #[error("音频内容为空")]
    EmptyAudio,
    #[error("音频文件超过 200 MiB")]
    AudioTooLarge,
    #[error("音频超过 15 分钟")]
    AudioTooLong,
    #[error("SILK 语音数据无效")]
    InvalidSilk,
    #[error("SILK 语音解码失败")]
    SilkDecode,
    #[error("FFmpeg 无法启动")]
    FfmpegUnavailable,
    #[error("音频格式转换失败")]
    AudioConversion,
    #[error("音频转换超时")]
    ConversionTimeout,
    #[error("语音识别服务配置不完整")]
    NotConfigured,
    #[error("语音识别服务请求失败")]
    Request,
    #[error("语音识别服务 HTTP {0}")]
    HttpStatus(u16),
    #[error("语音识别响应过大或格式无效")]
    InvalidResponse,
}

pub fn pcm_to_wav(pcm: &[u8]) -> Result<Vec<u8>, AsrError> {
    if pcm.is_empty() {
        return Err(AsrError::EmptyAudio);
    }
    if pcm.len() > MAX_PCM_BYTES {
        return Err(AsrError::AudioTooLong);
    }
    let data_size = u32::try_from(pcm.len()).map_err(|_| AsrError::AudioTooLong)?;
    let riff_size = data_size.checked_add(36).ok_or(AsrError::AudioTooLong)?;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_size.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&16_000_u32.to_le_bytes());
    wav.extend_from_slice(&32_000_u32.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    wav.extend_from_slice(pcm);
    Ok(wav)
}

/// Decode QQ SILK framing before handing regular media formats to FFmpeg.
/// SILK packets are preflighted so the decoder cannot allocate beyond the 15-minute PCM budget.
pub async fn audio_buffer_to_pcm(
    bytes: &[u8],
    name: &str,
    ffmpeg_path: &OsStr,
) -> Result<Vec<u8>, AsrError> {
    if bytes.is_empty() {
        return Err(AsrError::EmptyAudio);
    }
    if bytes.len() > MAX_AUDIO_BYTES {
        return Err(AsrError::AudioTooLarge);
    }
    if has_silk_header(bytes) {
        let frames = silk_frame_count(bytes)?;
        let estimated = frames.checked_mul(640 * 2).ok_or(AsrError::AudioTooLong)?;
        if estimated > MAX_PCM_BYTES {
            return Err(AsrError::AudioTooLong);
        }
        let decoded = silk_rs::decode_silk(bytes, 16_000).map_err(|_| AsrError::SilkDecode)?;
        if decoded.is_empty() {
            return Err(AsrError::EmptyAudio);
        }
        if decoded.len() > MAX_PCM_BYTES {
            return Err(AsrError::AudioTooLong);
        }
        return Ok(decoded);
    }
    ffmpeg_to_pcm(bytes, name, ffmpeg_path).await
}

fn has_silk_header(bytes: &[u8]) -> bool {
    let bytes = bytes.strip_prefix(b"\x02").unwrap_or(bytes);
    bytes.starts_with(b"#!SILK_V3")
}

fn silk_frame_count(bytes: &[u8]) -> Result<usize, AsrError> {
    let bytes = bytes.strip_prefix(b"\x02").unwrap_or(bytes);
    let mut input = bytes
        .strip_prefix(b"#!SILK_V3")
        .ok_or(AsrError::InvalidSilk)?;
    let mut frames = 0_usize;
    while !input.is_empty() {
        if input.len() < 2 {
            return Err(AsrError::InvalidSilk);
        }
        let packet_len = usize::from(u16::from_le_bytes([input[0], input[1]]));
        input = &input[2..];
        if packet_len == 0 || packet_len > 640 || input.len() < packet_len {
            return Err(AsrError::InvalidSilk);
        }
        input = &input[packet_len..];
        frames = frames.checked_add(1).ok_or(AsrError::AudioTooLong)?;
        if frames > MAX_PCM_BYTES / (640 * 2) {
            return Err(AsrError::AudioTooLong);
        }
    }
    if frames == 0 {
        return Err(AsrError::InvalidSilk);
    }
    Ok(frames)
}

struct TemporaryAudioInput(PathBuf);

impl TemporaryAudioInput {
    fn write(bytes: &[u8], name: &str) -> Result<Self, AsrError> {
        let extension = std::path::Path::new(name)
            .extension()
            .and_then(OsStr::to_str)
            .filter(|extension| {
                !extension.is_empty()
                    && extension.len() <= 8
                    && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
            .unwrap_or("media");
        let path = std::env::temp_dir().join(format!(
            "rinbot-audio-{}.{}",
            uuid::Uuid::new_v4(),
            extension
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(|_| AsrError::AudioConversion)?;
        if file.write_all(bytes).is_err() {
            let _ = fs::remove_file(&path);
            return Err(AsrError::AudioConversion);
        }
        Ok(Self(path))
    }
}

impl Drop for TemporaryAudioInput {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

async fn ffmpeg_to_pcm(bytes: &[u8], name: &str, ffmpeg_path: &OsStr) -> Result<Vec<u8>, AsrError> {
    let input = TemporaryAudioInput::write(bytes, name)?;
    let mut child = Command::new(ffmpeg_path)
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&input.0)
        .args(["-vn", "-ar", "16000", "-ac", "1", "-f", "s16le", "pipe:1"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| AsrError::FfmpegUnavailable)?;
    let mut stdout = child.stdout.take().ok_or(AsrError::AudioConversion)?;
    let mut stderr = child.stderr.take().ok_or(AsrError::AudioConversion)?;
    let stderr_drain = tokio::spawn(async move {
        let mut chunk = [0_u8; 1024];
        let mut retained = Vec::with_capacity(512);
        loop {
            let count = stderr.read(&mut chunk).await.unwrap_or(0);
            if count == 0 {
                break;
            }
            let remaining = 512_usize.saturating_sub(retained.len());
            retained.extend_from_slice(&chunk[..count.min(remaining)]);
        }
    });
    let work = async move {
        let mut pcm = Vec::new();
        let mut chunk = [0_u8; 16 * 1024];
        loop {
            let count = stdout
                .read(&mut chunk)
                .await
                .map_err(|_| AsrError::AudioConversion)?;
            if count == 0 {
                break;
            }
            if pcm.len().saturating_add(count) > MAX_PCM_BYTES {
                let _ = child.kill().await;
                let _ = child.wait().await;
                let _ = stderr_drain.await;
                return Err(AsrError::AudioTooLong);
            }
            pcm.extend_from_slice(&chunk[..count]);
        }
        let status = child.wait().await.map_err(|_| AsrError::AudioConversion)?;
        let _ = stderr_drain.await;
        if !status.success() {
            return Err(AsrError::AudioConversion);
        }
        if pcm.is_empty() {
            return Err(AsrError::EmptyAudio);
        }
        Ok(pcm)
    };
    tokio::time::timeout(Duration::from_secs(10 * 60), work)
        .await
        .map_err(|_| AsrError::ConversionTimeout)?
}

pub async fn transcribe_openai_compatible(
    settings: &AsrSettings,
    api_key: &str,
    wav: Vec<u8>,
) -> Result<String, AsrError> {
    if !settings.configured(api_key) {
        return Err(AsrError::NotConfigured);
    }
    if wav.len() > OPENAI_WAV_MAX_BYTES {
        return Err(AsrError::AudioTooLong);
    }
    let endpoint = format!(
        "{}/audio/transcriptions",
        settings.base_url.trim().trim_end_matches('/')
    );
    let mut form = multipart::Form::new()
        .part(
            "file",
            multipart::Part::bytes(wav)
                .file_name("audio.wav")
                .mime_str("audio/wav")
                .map_err(|_| AsrError::Request)?,
        )
        .text("model", settings.model.trim().to_owned());
    if !settings.language.trim().is_empty() {
        form = form.text("language", settings.language.trim().to_owned());
    }
    let response = Client::new()
        .post(endpoint)
        .bearer_auth(api_key)
        .timeout(Duration::from_millis(settings.timeout_ms))
        .multipart(form)
        .send()
        .await
        .map_err(|_| AsrError::Request)?;
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|length| length > 2 * 1024 * 1024)
    {
        return Err(AsrError::InvalidResponse);
    }
    let body = response.bytes().await.map_err(|_| AsrError::Request)?;
    if body.len() > 2 * 1024 * 1024 {
        return Err(AsrError::InvalidResponse);
    }
    if !status.is_success() {
        return Err(AsrError::HttpStatus(status.as_u16()));
    }
    let value = serde_json::from_slice::<serde_json::Value>(&body)
        .map_err(|_| AsrError::InvalidResponse)?;
    let text = value
        .get("text")
        .or_else(|| value.get("result").and_then(|result| result.get("text")))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    Ok(text)
}

pub fn speech_caveat(pcm: &[u8]) -> String {
    if pcm.len() < 3 * PCM_BYTES_PER_SECOND || pcm.len() % 2 != 0 {
        return String::new();
    }
    let frame_bytes = 640_usize;
    let rms = pcm
        .chunks(frame_bytes)
        .filter(|frame| frame.len() >= 2)
        .map(|frame| {
            let mut energy = 0.0_f64;
            let mut samples = 0_u32;
            for sample in frame.chunks_exact(2) {
                let value = f64::from(i16::from_le_bytes([sample[0], sample[1]])) / 32768.0;
                energy += value * value;
                samples += 1;
            }
            (energy / f64::from(samples.max(1))).sqrt()
        })
        .collect::<Vec<_>>();
    let peak = rms.iter().copied().fold(0.0_f64, f64::max);
    if peak <= 0.0 {
        return String::new();
    }
    let quiet = rms.iter().filter(|value| **value < peak * 0.06).count();
    if quiet as f64 / (rms.len() as f64) < 0.05 {
        "（这段音频几乎没有停顿，可能主要是音乐/音效而不是人声；自动识别可能不准确，请勿把转写当作确定事实。）".into()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn openai_compatible_request_sends_bounded_multipart_and_parses_transcript() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            let body_start;
            loop {
                let count = stream.read(&mut chunk).await.unwrap();
                assert!(count > 0, "client closed before request completed");
                request.extend_from_slice(&chunk[..count]);
                if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    body_start = index + 4;
                    break;
                }
            }
            let headers = String::from_utf8_lossy(&request[..body_start]).into_owned();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .expect("multipart content length");
            while request.len() - body_start < content_length {
                let count = stream.read(&mut chunk).await.unwrap();
                assert!(count > 0, "truncated multipart request");
                request.extend_from_slice(&chunk[..count]);
            }
            let body = &request[body_start..body_start + content_length];
            assert!(headers.starts_with("POST /v1/audio/transcriptions HTTP/1.1"));
            assert!(headers
                .to_ascii_lowercase()
                .contains("authorization: bearer test-asr-key"));
            let body = String::from_utf8_lossy(body);
            assert!(body.contains("name=\"model\""));
            assert!(body.contains("whisper-test"));
            assert!(body.contains("name=\"language\""));
            assert!(body.contains("zh"));
            assert!(body.contains("filename=\"audio.wav\""));
            assert!(body.contains("RIFF"));
            let response_body = "{\"text\":\"测试转写\"}".as_bytes();
            let response_headers = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                response_body.len()
            );
            stream.write_all(response_headers.as_bytes()).await.unwrap();
            stream.write_all(response_body).await.unwrap();
        });
        let settings = AsrSettings {
            enabled: true,
            provider: "openai".into(),
            base_url: format!("http://{address}/v1"),
            model: "whisper-test".into(),
            language: "zh".into(),
            max_per_hour: 12,
            timeout_ms: 5_000,
        };
        let wav = pcm_to_wav(&[0; 320]).unwrap();
        let transcript = transcribe_openai_compatible(&settings, "test-asr-key", wav)
            .await
            .unwrap();
        assert_eq!(transcript, "测试转写");
        server.await.unwrap();
    }

    #[test]
    fn wav_header_is_mono_16khz_and_exactly_sized() {
        let wav = pcm_to_wav(&[1, 2, 3, 4]).unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 4);
        assert_eq!(wav.len(), 48);
    }

    #[test]
    fn settings_validate_provider_bounds_and_never_treat_missing_credentials_as_ready() {
        let settings = AsrSettings {
            enabled: true,
            provider: "openai".into(),
            base_url: "https://api.example.invalid/v1".into(),
            model: "whisper-large".into(),
            language: "zh".into(),
            max_per_hour: 12,
            timeout_ms: 300_000,
        };
        assert!(settings.validate().is_ok());
        assert!(!settings.configured(""));
        assert!(settings.configured("secret"));
        let mut unsafe_settings = settings;
        unsafe_settings.base_url = "http://user:pass@localhost/v1".into();
        assert!(unsafe_settings.validate().is_err());
    }

    #[test]
    fn silk_preflight_bounds_frames_and_rejects_truncated_packets() {
        let valid_one_frame = [b"#!SILK_V3".as_slice(), &[2, 0, 0, 0]].concat();
        assert_eq!(silk_frame_count(&valid_one_frame).unwrap(), 1);
        let truncated = [b"#!SILK_V3".as_slice(), &[5, 0, 1, 2]].concat();
        assert!(matches!(
            silk_frame_count(&truncated),
            Err(AsrError::InvalidSilk)
        ));
    }

    #[test]
    fn silence_does_not_generate_speech_caveat() {
        assert!(speech_caveat(&vec![0; 4 * PCM_BYTES_PER_SECOND]).is_empty());
    }

    #[test]
    fn hourly_quota_is_bounded_and_rolls_over_at_epoch_hour() {
        let quota = AsrQuota::default();
        assert!(quota.consume(3_599_999, 1));
        assert!(!quota.consume(3_599_999, 1));
        assert!(quota.consume(3_600_000, 1));
    }
}
