use std::{sync::Arc, time::Duration};

use clap::Parser;
use http::{header, HeaderValue};
use rinbot::{
    adapters::{
        napcat::{NapcatAdapter, NapcatConfig},
        sqlite::SqliteStore,
    },
    api::{router, ApiEvent, AppState, RuntimeState},
    application::router::bounded_inbox_queue,
    domain::event::DomainEvent,
    runtime::config::Cli,
};
use serde_json::json;
use tokio::{
    net::TcpListener,
    sync::{broadcast, mpsc},
};
use tower_http::{services::ServeDir, set_header::SetResponseHeaderLayer};
use tracing::{error, info, warn};
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let cli = Arc::new(Cli::parse());
    if !is_loopback_host(&cli.host) && cli.api_token.trim().is_empty() {
        return Err("non-loopback API binding requires --api-token or QQBOTD_API_TOKEN".into());
    }
    let store = Arc::new(SqliteStore::open(cli.data_dir.join("messages.sqlite"))?);
    let recovered = store.recover_expired_inbox()?;
    if recovered > 0 {
        info!(recovered, "requeued expired inbox leases");
    }
    let runtime = Arc::new(RuntimeState::new());
    let (events, _) = broadcast::channel::<ApiEvent>(128);
    let state = AppState {
        store: store.clone(),
        runtime: runtime.clone(),
        events,
        config: cli.clone(),
    };
    let app = router(state.clone())
        .fallback_service(ServeDir::new(cli.ui_dir.clone()))
        // The HTML entry point references hashed bundles. Revalidate it so a
        // deployment cannot leave the browser with a missing old bundle.
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache, must-revalidate"),
        ));

    let (domain_tx, mut domain_rx) = mpsc::channel::<DomainEvent>(64);
    let (inbox_tx, mut inbox_rx) = bounded_inbox_queue(cli.queue_capacity);
    let ingestion_state = state.clone();
    tokio::spawn(async move {
        while let Some(event) = domain_rx.recv().await {
            match event {
                DomainEvent::Message(message) => {
                    match ingestion_state.store.insert_incoming(&message) {
                        Ok(Some(inbox_id)) => {
                            ingestion_state
                                .runtime
                                .queue_depth
                                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                            if inbox_tx.send(inbox_id).await.is_err() {
                                error!(
                                    inbox_id,
                                    "inbox queue closed; durable inbox row remains queued"
                                );
                                break;
                            }
                            ingestion_state.publish(
                                "message.received",
                                json!({
                                    "conversationId": message.conversation_id,
                                    "messageId": message.message_id,
                                    "inboxId": inbox_id,
                                }),
                            );
                        }
                        Ok(None) => {
                            info!(message_id = %message.message_id, "duplicate OneBot message ignored")
                        }
                        Err(error) => error!(%error, "failed to persist inbound message"),
                    }
                }
                DomainEvent::Unknown {
                    post_type,
                    event_type,
                } => {
                    let payload = json!({ "postType": post_type, "eventType": event_type });
                    if let Err(error) = ingestion_state
                        .store
                        .record_runtime_event("onebot.unknown", &payload)
                    {
                        error!(%error, "failed to persist unknown OneBot event");
                    }
                    ingestion_state.publish("onebot.unknown", payload);
                }
            }
        }
    });

    let worker_state = state.clone();
    tokio::spawn(async move {
        while let Some(inbox_id) = inbox_rx.recv().await {
            worker_state
                .runtime
                .queue_depth
                .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            let lease_id = format!("worker-{}", Uuid::new_v4());
            match worker_state.store.claim_inbox(inbox_id, &lease_id, 30_000) {
                Ok(true) => {
                    if let Err(error) = worker_state
                        .store
                        .complete_inbox(inbox_id, &lease_id, "observed", None)
                    {
                        error!(%error, inbox_id, "failed to complete observation inbox row");
                    }
                }
                Ok(false) => info!(inbox_id, "inbox row was already claimed or completed"),
                Err(error) => error!(%error, inbox_id, "failed to claim observation inbox row"),
            }
        }
    });

    if cli.connect_onebot {
        let adapter = Arc::new(NapcatAdapter::new(NapcatConfig {
            ws_url: cli.onebot_ws_url.clone(),
            http_url: cli.onebot_http_url.clone(),
            access_token: cli.onebot_token.clone(),
            observe_only: true,
        }));
        let connector_state = state.clone();
        let status_adapter = adapter.clone();
        let status_state = state.clone();
        tokio::spawn(async move {
            loop {
                status_state.runtime.onebot_connected.store(
                    status_adapter.connected(),
                    std::sync::atomic::Ordering::Release,
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        });
        tokio::spawn(async move {
            let mut reconnect_delay = Duration::from_secs(3);
            loop {
                match adapter.run(domain_tx.clone()).await {
                    Ok(()) => warn!("NapCat adapter stopped"),
                    Err(error) => {
                        warn!(%error, "NapCat adapter disconnected; observation retry scheduled")
                    }
                }
                connector_state
                    .runtime
                    .onebot_connected
                    .store(false, std::sync::atomic::Ordering::Release);
                connector_state.publish("onebot.disconnected", json!({}));
                tokio::time::sleep(reconnect_delay).await;
                reconnect_delay = (reconnect_delay * 2).min(Duration::from_secs(30));
            }
        });
    } else {
        info!("NapCat connection disabled; use --connect-onebot only with an isolated observation setup");
    }

    let bind = format!("{}:{}", cli.host, cli.port);
    let listener = TcpListener::bind(&bind).await?;
    info!(%bind, data_dir = ?cli.data_dir, "RInBot API ready");
    axum::serve(listener, app).await?;
    Ok(())
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host.trim(), "127.0.0.1" | "localhost" | "::1")
}
