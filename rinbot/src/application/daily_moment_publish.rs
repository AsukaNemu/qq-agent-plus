//! Durable Daily Moment publication executor.
//!
//! The ledger claim is committed before the QZone port is called. Any result
//! that cannot be proven synchronously is quarantined and is never replayed.

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use thiserror::Error;

use crate::{
    application::daily_moment_generation::DailyMomentImageRef,
    daily_moments::{DailyMomentsError, DailyMomentsSettings, DailyMomentsStore},
    ports::gateway::{DailyMomentImageReader, QzoneGateway, QzoneReadAction},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DailyMomentPublishOutcome {
    NotClaimed,
    Deduplicated { tid: String },
    Published { tid: String },
    Unknown,
}

#[derive(Debug, Error)]
pub enum DailyMomentPublishError {
    #[error("Daily Moments settings are invalid or disabled")]
    Settings,
    #[error(transparent)]
    Store(#[from] DailyMomentsError),
}

pub async fn publish_daily_moment(
    store: &mut DailyMomentsStore,
    gateway: &dyn QzoneGateway,
    record_id: &str,
    slot_id: &str,
    settings: &DailyMomentsSettings,
    image_reader: Option<&dyn DailyMomentImageReader>,
) -> Result<DailyMomentPublishOutcome, DailyMomentPublishError> {
    settings
        .validate()
        .map_err(|_| DailyMomentPublishError::Settings)?;
    if !settings.enabled {
        return Err(DailyMomentPublishError::Settings);
    }
    let Some(preview) = store.record(record_id)? else {
        return Ok(DailyMomentPublishOutcome::NotClaimed);
    };
    let selected_image_ids = preview
        .get("imageIds")
        .and_then(Value::as_array)
        .map(|images| {
            images
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut images = Vec::new();
    let mut image_errors = Vec::new();
    if settings.allow_images {
        for image_id in selected_image_ids.iter().take(settings.max_images as usize) {
            let image_ref = preview
                .get("imageRefs")
                .and_then(Value::as_array)
                .and_then(|refs| refs.iter().find(|image| image["id"] == *image_id))
                .and_then(|image| {
                    serde_json::from_value::<DailyMomentImageRef>(image.clone()).ok()
                });
            let Some(image_ref) = image_ref else {
                image_errors.push(format!("{image_id}: 图片来源已失效，本次不配此图"));
                continue;
            };
            let Some(reader) = image_reader else {
                image_errors.push(format!("{image_id}: 图片读取不可用，本次不配此图"));
                continue;
            };
            let upload_source = if let Some(sticker_id) = &image_ref.sticker_id {
                reader.daily_moment_sticker_upload_source(sticker_id).await
            } else {
                reader
                    .daily_moment_upload_source(
                        &image_ref.conversation_id,
                        &image_ref.message_id,
                        image_ref.media_index,
                    )
                    .await
            };
            match upload_source {
                Ok(Some(source)) if valid_upload_source(&source) => images.push(source),
                Ok(Some(_)) => {
                    image_errors.push(format!("{image_id}: 图片格式或大小不受支持，本次不配此图"))
                }
                Ok(None) | Err(_) => {
                    image_errors.push(format!("{image_id}: 图片来源已失效，本次不配此图"))
                }
            }
        }
    }
    if selected_image_ids.len() > settings.max_images as usize {
        return Err(DailyMomentPublishError::Settings);
    }
    let content = preview
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if let Ok(recent) = gateway
        .qzone_read(QzoneReadAction::GetMessageList, json!({"pos":0,"num":30}))
        .await
    {
        let duplicate_tid = recent
            .get("msglist")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find_map(|post| {
                let same_content = post.get("content").and_then(Value::as_str) == Some(content);
                let tid = post
                    .get("tid")
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| value.to_string())
                    })
                    .filter(|tid| valid_tid(tid));
                if same_content {
                    tid
                } else {
                    None
                }
            });
        if let Some(tid) = duplicate_tid {
            if store.mark_daily_moment_deduplicated(record_id, slot_id, &tid)? {
                return Ok(DailyMomentPublishOutcome::Deduplicated { tid });
            }
            return Ok(DailyMomentPublishOutcome::NotClaimed);
        }
    }
    let Some(record) =
        store.claim_daily_moment_publication(record_id, slot_id, images.len(), &image_errors)?
    else {
        return Ok(DailyMomentPublishOutcome::NotClaimed);
    };
    let content = record
        .get("content")
        .and_then(serde_json::Value::as_str)
        .ok_or(DailyMomentsError::Invalid(
            "claimed record content is invalid".into(),
        ))?;
    let mut params = json!({
        "content": content,
        "images": images,
        "ugc_right": settings.visibility
    });
    if [16, 128].contains(&settings.visibility) {
        params["target_uins"] = json!(settings.target_uins);
    }

    match gateway.publish_daily_moment(params).await {
        Ok(tid) => {
            if !valid_tid(&tid) {
                store.finish_daily_moment_publication(
                    record_id,
                    slot_id,
                    None,
                    "QZone 未返回可核验的动态编号；需要人工核对，不会自动重试",
                )?;
                return Ok(DailyMomentPublishOutcome::Unknown);
            }
            let completed =
                store.finish_daily_moment_publication(record_id, slot_id, Some(&tid), "")?;
            if completed {
                Ok(DailyMomentPublishOutcome::Published { tid })
            } else {
                Ok(DailyMomentPublishOutcome::Unknown)
            }
        }
        Err(_error) => {
            store.finish_daily_moment_publication(
                record_id,
                slot_id,
                None,
                "QZone 发布结果未能确认；需要人工核对，不会自动重试",
            )?;
            Ok(DailyMomentPublishOutcome::Unknown)
        }
    }
}

fn valid_upload_source(source: &str) -> bool {
    let Some(encoded) = source.strip_prefix("base64://") else {
        return false;
    };
    if encoded.is_empty() || encoded.len() > 12 * 1024 * 1024 {
        return false;
    }
    STANDARD
        .decode(encoded)
        .is_ok_and(|bytes| !bytes.is_empty() && bytes.len() <= 8 * 1024 * 1024)
}

fn valid_tid(tid: &str) -> bool {
    !tid.is_empty()
        && tid.len() <= 256
        && tid
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use serde_json::{json, Value};
    use tempfile::tempdir;

    use super::*;
    use crate::ports::gateway::{GatewayError, QzoneReadAction, QzoneWriteAction};

    struct FakeGateway {
        result: Result<String, GatewayError>,
        recent: Value,
        writes: AtomicUsize,
        params: std::sync::Mutex<Vec<Value>>,
    }

    #[async_trait]
    impl QzoneGateway for FakeGateway {
        async fn qzone_read(
            &self,
            _action: QzoneReadAction,
            _params: Value,
        ) -> Result<Value, GatewayError> {
            Ok(self.recent.clone())
        }

        async fn qzone_write(
            &self,
            _action: QzoneWriteAction,
            _params: Value,
        ) -> Result<(), GatewayError> {
            unreachable!("daily moment publisher must use receipt-returning port")
        }

        async fn publish_daily_moment(&self, params: Value) -> Result<String, GatewayError> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.params.lock().unwrap().push(params);
            match &self.result {
                Ok(tid) => Ok(tid.clone()),
                Err(GatewayError::UnknownOutcome) => Err(GatewayError::UnknownOutcome),
                Err(GatewayError::Request(message)) => Err(GatewayError::Request(message.clone())),
                Err(GatewayError::UnsupportedAction(action)) => {
                    Err(GatewayError::UnsupportedAction(action.clone()))
                }
                Err(GatewayError::WritesDisabled) => Err(GatewayError::WritesDisabled),
                Err(GatewayError::NotConnected) => Err(GatewayError::NotConnected),
            }
        }
    }

    struct FakeImageReader;

    #[async_trait]
    impl DailyMomentImageReader for FakeImageReader {
        async fn daily_moment_image(
            &self,
            conversation_id: &str,
            message_id: &str,
            media_index: usize,
        ) -> Result<Option<String>, GatewayError> {
            assert_eq!(conversation_id, "group:123456");
            assert_eq!(message_id, "message-1");
            assert_eq!(media_index, 0);
            Ok(Some(format!(
                "data:image/jpeg;base64,{}",
                STANDARD.encode(b"fake-jpeg")
            )))
        }

        async fn daily_moment_upload_source(
            &self,
            conversation_id: &str,
            message_id: &str,
            media_index: usize,
        ) -> Result<Option<String>, GatewayError> {
            assert_eq!(conversation_id, "group:123456");
            assert_eq!(message_id, "message-1");
            assert_eq!(media_index, 0);
            Ok(Some(format!(
                "base64://{}",
                STANDARD.encode(b"GIF89a-fake-animation")
            )))
        }

        async fn daily_moment_sticker_upload_source(
            &self,
            sticker_id: &str,
        ) -> Result<Option<String>, GatewayError> {
            assert_eq!(sticker_id, "favorite-1");
            Ok(Some(format!(
                "base64://{}",
                STANDARD.encode(b"GIF89a-sticker-animation")
            )))
        }
    }

    fn store_with_ready_preview(
        path: &std::path::Path,
        image_source: Option<&str>,
    ) -> (DailyMomentsStore, String, String) {
        let mut store = DailyMomentsStore::open(path).unwrap();
        let windows = [crate::daily_moments_schedule::MomentScheduleWindow {
            start: "17:00".into(),
            end: "18:00".into(),
            count: 1,
        }];
        let now = 1_789_372_800_000_i64;
        store
            .update_schedule_plan(Some(&windows), now, || 0.0, false, false)
            .unwrap();
        let (slots, _) = store.list_schedule_slots(10, 0).unwrap();
        let due_slot = slots
            .iter()
            .min_by_key(|slot| slot["at"].as_i64().unwrap())
            .unwrap();
        let slot_id = due_slot["id"].as_str().unwrap().to_owned();
        let day_key = due_slot["dayKey"].as_str().unwrap().to_owned();
        store
            .claim_due_schedule_slot(due_slot["at"].as_i64().unwrap())
            .unwrap()
            .unwrap();
        let decision = json!({
            "decision":"publish",
            "content":"可发布的正文",
            "imageIds":if image_source.is_some() { json!(["image-1"]) } else { json!([]) },
            "imageRefs":if image_source == Some("message") {
                json!([{
                    "id":"image-1",
                    "conversationId":"group:123456",
                    "messageId":"message-1",
                    "mediaIndex":0
                }])
            } else if image_source == Some("sticker") {
                json!([{
                    "id":"image-1",
                    "conversationId":"",
                    "messageId":"",
                    "mediaIndex":0,
                    "stickerId":"favorite-1"
                }])
            } else { json!([]) },
            "groupSummaries":[]
        });
        store
            .persist_generation_decision(&slot_id, &day_key, "draft", &decision)
            .unwrap();
        let record_id = format!("rust-{slot_id}");
        store
            .complete_generated_slot(&slot_id, "draft-ready", &record_id)
            .unwrap();
        (store, record_id, slot_id)
    }

    #[tokio::test]
    async fn confirmed_publication_persists_before_replay_and_uses_visibility_params() {
        let directory = tempdir().unwrap();
        let (mut store, record_id, slot_id) =
            store_with_ready_preview(&directory.path().join("daily.sqlite"), None);
        let gateway = FakeGateway {
            result: Ok("tid-123".into()),
            recent: Value::Null,
            writes: AtomicUsize::new(0),
            params: std::sync::Mutex::new(Vec::new()),
        };
        let settings = DailyMomentsSettings {
            enabled: true,
            visibility: 16,
            target_uins: vec!["12345678".into()],
            ..Default::default()
        };
        assert_eq!(
            publish_daily_moment(&mut store, &gateway, &record_id, &slot_id, &settings, None)
                .await
                .unwrap(),
            DailyMomentPublishOutcome::Published {
                tid: "tid-123".into()
            }
        );
        assert_eq!(gateway.writes.load(Ordering::SeqCst), 1);
        assert_eq!(gateway.params.lock().unwrap()[0]["ugc_right"], 16);
        assert_eq!(
            gateway.params.lock().unwrap()[0]["target_uins"],
            json!(["12345678"])
        );
        assert_eq!(
            publish_daily_moment(&mut store, &gateway, &record_id, &slot_id, &settings, None)
                .await
                .unwrap(),
            DailyMomentPublishOutcome::NotClaimed
        );
        assert_eq!(gateway.writes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn uncertain_gateway_response_is_quarantined_without_retry() {
        let directory = tempdir().unwrap();
        let (mut store, record_id, slot_id) =
            store_with_ready_preview(&directory.path().join("daily.sqlite"), None);
        let gateway = FakeGateway {
            result: Err(GatewayError::UnknownOutcome),
            recent: Value::Null,
            writes: AtomicUsize::new(0),
            params: std::sync::Mutex::new(Vec::new()),
        };
        let settings = DailyMomentsSettings {
            enabled: true,
            ..Default::default()
        };
        assert_eq!(
            publish_daily_moment(&mut store, &gateway, &record_id, &slot_id, &settings, None)
                .await
                .unwrap(),
            DailyMomentPublishOutcome::Unknown
        );
        assert_eq!(
            store.record(&record_id).unwrap().unwrap()["status"],
            "publish-unknown"
        );
        assert_eq!(
            publish_daily_moment(&mut store, &gateway, &record_id, &slot_id, &settings, None)
                .await
                .unwrap(),
            DailyMomentPublishOutcome::NotClaimed
        );
        assert_eq!(gateway.writes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn selected_image_is_resolved_fresh_and_sent_as_bounded_base64() {
        let directory = tempdir().unwrap();
        let (mut store, record_id, slot_id) =
            store_with_ready_preview(&directory.path().join("daily.sqlite"), Some("message"));
        let gateway = FakeGateway {
            result: Ok("tid-image".into()),
            recent: Value::Null,
            writes: AtomicUsize::new(0),
            params: std::sync::Mutex::new(Vec::new()),
        };
        let settings = DailyMomentsSettings {
            enabled: true,
            max_images: 1,
            ..Default::default()
        };
        assert_eq!(
            publish_daily_moment(
                &mut store,
                &gateway,
                &record_id,
                &slot_id,
                &settings,
                Some(&FakeImageReader),
            )
            .await
            .unwrap(),
            DailyMomentPublishOutcome::Published {
                tid: "tid-image".into()
            }
        );
        let params = gateway.params.lock().unwrap();
        assert_eq!(
            params[0]["images"][0],
            format!("base64://{}", STANDARD.encode(b"GIF89a-fake-animation"))
        );
        let record = store.record(&record_id).unwrap().unwrap();
        assert_eq!(record["imageCount"], 1);
        assert_eq!(record["imageErrors"], json!([]));
    }

    #[tokio::test]
    async fn selected_sticker_is_resolved_fresh_and_sent_as_bounded_base64() {
        let directory = tempdir().unwrap();
        let (mut store, record_id, slot_id) =
            store_with_ready_preview(&directory.path().join("daily.sqlite"), Some("sticker"));
        let gateway = FakeGateway {
            result: Ok("tid-sticker".into()),
            recent: Value::Null,
            writes: AtomicUsize::new(0),
            params: std::sync::Mutex::new(Vec::new()),
        };
        let settings = DailyMomentsSettings {
            enabled: true,
            max_images: 1,
            ..Default::default()
        };
        assert_eq!(
            publish_daily_moment(
                &mut store,
                &gateway,
                &record_id,
                &slot_id,
                &settings,
                Some(&FakeImageReader),
            )
            .await
            .unwrap(),
            DailyMomentPublishOutcome::Published {
                tid: "tid-sticker".into()
            }
        );
        let params = gateway.params.lock().unwrap();
        assert_eq!(
            params[0]["images"][0],
            format!("base64://{}", STANDARD.encode(b"GIF89a-sticker-animation"))
        );
        assert_eq!(store.record(&record_id).unwrap().unwrap()["imageCount"], 1);
    }

    #[tokio::test]
    async fn exact_recent_post_is_deduplicated_without_attempting_a_write() {
        let directory = tempdir().unwrap();
        let (mut store, record_id, slot_id) =
            store_with_ready_preview(&directory.path().join("daily.sqlite"), None);
        let gateway = FakeGateway {
            result: Ok("must-not-be-used".into()),
            recent: json!({"msglist":[{"tid":"existing-1","content":"可发布的正文"}]}),
            writes: AtomicUsize::new(0),
            params: std::sync::Mutex::new(Vec::new()),
        };
        let settings = DailyMomentsSettings {
            enabled: true,
            ..Default::default()
        };
        assert_eq!(
            publish_daily_moment(&mut store, &gateway, &record_id, &slot_id, &settings, None)
                .await
                .unwrap(),
            DailyMomentPublishOutcome::Deduplicated {
                tid: "existing-1".into()
            }
        );
        assert_eq!(gateway.writes.load(Ordering::SeqCst), 0);
        let record = store.record(&record_id).unwrap().unwrap();
        assert_eq!(record["status"], "published");
        assert_eq!(record["publishAttempted"], false);
        assert_eq!(record["deduplicated"], true);
    }
}
