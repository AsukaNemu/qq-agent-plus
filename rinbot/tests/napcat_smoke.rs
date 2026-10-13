use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use futures_util::SinkExt;
use rinbot::{
    adapters::napcat::{NapcatAdapter, NapcatConfig},
    domain::event::DomainEvent,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        handshake::server::{Request, Response},
        Message,
    },
};

#[tokio::test]
async fn isolated_napcat_smoke_authenticates_reads_login_and_normalizes_events() {
    let http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr = http_listener.local_addr().unwrap();
    let http_authenticated = Arc::new(AtomicBool::new(false));
    let http_authenticated_task = http_authenticated.clone();
    let http_task = tokio::spawn(async move {
        let (mut stream, _) = http_listener.accept().await.unwrap();
        let mut request = vec![0; 4096];
        let length = stream.read(&mut request).await.unwrap();
        let request = String::from_utf8_lossy(&request[..length]);
        http_authenticated_task.store(
            request.starts_with("POST /get_login_info ")
                && request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer smoke-token"),
            Ordering::Release,
        );
        let body =
            r#"{"status":"ok","retcode":0,"data":{"user_id":12345,"nickname":"RInBot Test"}}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(), body
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });

    let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_addr = ws_listener.local_addr().unwrap();
    let ws_authenticated = Arc::new(AtomicBool::new(false));
    let ws_authenticated_task = ws_authenticated.clone();
    let ws_task = tokio::spawn(async move {
        let (stream, _) = ws_listener.accept().await.unwrap();
        let mut socket = accept_hdr_async(stream, move |request: &Request, response: Response| {
            ws_authenticated_task.store(
                request
                    .headers()
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    == Some("Bearer smoke-token"),
                Ordering::Release,
            );
            Ok(response)
        })
        .await
        .unwrap();
        let event = include_str!("fixtures/private-text.json");
        socket.send(Message::Text(event.into())).await.unwrap();
        socket.send(Message::Close(None)).await.unwrap();
    });

    let adapter = NapcatAdapter::new(NapcatConfig {
        ws_url: format!("ws://{ws_addr}/onebot/v11/ws"),
        http_url: format!("http://{http_addr}"),
        access_token: "smoke-token".into(),
        observe_only: true,
    });
    let (event_tx, mut event_rx) = mpsc::channel(2);
    let run = tokio::time::timeout(Duration::from_secs(3), adapter.run(event_tx))
        .await
        .expect("isolated adapter should stop after the mock server closes");
    assert!(
        run.is_err(),
        "closed WebSocket must not look like a healthy persistent stream: {run:?}"
    );
    assert!(
        !adapter.connected(),
        "adapter must clear connected state after disconnect"
    );

    let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
        .await
        .unwrap()
        .unwrap();
    match event {
        DomainEvent::Message(message) => {
            assert_eq!(message.conversation_id, "private:2001");
            assert_eq!(message.sender_id, "2001");
            assert_eq!(message.text_projection(), "hello RInBot");
        }
        other => panic!("expected normalized private message, got {other:?}"),
    }

    http_task.await.unwrap();
    ws_task.await.unwrap();
    assert!(
        http_authenticated.load(Ordering::Acquire),
        "HTTP action must carry Bearer token"
    );
    assert!(
        ws_authenticated.load(Ordering::Acquire),
        "WebSocket handshake must carry Bearer token"
    );
}
