use super::*;
use fiber_json_types::{ChannelSigningStatus, GetChannelSigningStatusResult};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

const CHANNEL_ID: Hash256 = Hash256([4; 32]);

pub(crate) async fn accept(
    socket: TcpStream,
    token: &str,
    channel_id: Hash256,
    subscription_id: Value,
) -> WebSocketStream<TcpStream> {
    let mut socket = tokio_tungstenite::accept_hdr_async(
        socket,
        |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
            assert_eq!(
                request.headers()["authorization"],
                format!("Bearer {token}")
            );
            assert!(!request.headers().contains_key("x-lsp-session-id"));
            Ok(response)
        },
    )
    .await
    .unwrap();
    let request: Value =
        serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(request["method"], "subscribe");
    assert_eq!(
        request["params"],
        json!(["channel_signing", {"channel_ids": [channel_id]}])
    );
    socket
        .send(Message::text(
            json!({"jsonrpc":"2.0", "id":request["id"], "result":subscription_id}).to_string(),
        ))
        .await
        .unwrap();
    socket
}

pub(crate) async fn notify(
    socket: &mut WebSocketStream<TcpStream>,
    subscription_id: Value,
    event: SubscriptionEvent,
) {
    socket
        .send(Message::text(
            json!({
                "jsonrpc":"2.0", "method":"subscribe",
                "params":{"subscription":subscription_id,"result":event}
            })
            .to_string(),
        ))
        .await
        .unwrap();
}

async fn disconnected(socket: &mut WebSocketStream<TcpStream>) {
    // Some clients send an unsubscribe/Close frame before dropping TCP.
    while let Some(Ok(message)) = socket.next().await {
        if matches!(message, Message::Close(_)) {
            return;
        }
    }
}

fn signing_event() -> SubscriptionEvent {
    SubscriptionEvent::ChannelSigning(Box::new(GetChannelSigningStatusResult {
        channel_id: CHANNEL_ID,
        status: ChannelSigningStatus::NoSignatureRequired,
    }))
}

#[test]
fn websocket_urls_require_ws_or_wss_and_never_embed_credentials() {
    for url in ["ws://127.0.0.1:21717", "wss://lsp.example/rpc"] {
        assert!(parse_url(url).is_ok());
    }
    for url in [
        "http://127.0.0.1:21716",
        "ws://user:secret@localhost",
        "ws://localhost?token=secret",
        "ws://localhost#secret",
        "invalid",
    ] {
        assert!(!format!("{:#}", parse_url(url).unwrap_err()).contains("secret"));
    }
}

#[tokio::test]
async fn reconnects_after_processing_failure_and_disconnect_and_replays_pending_work() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for id in [json!("first"), json!(42), json!("replayed")] {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = accept(socket, "tenant-token", CHANNEL_ID, id.clone()).await;
            // Notifications for another subscription must never reach this signer.
            notify(
                &mut socket,
                json!("unrelated"),
                SubscriptionEvent::Removed {
                    channel_id: CHANNEL_ID,
                },
            )
            .await;
            notify(&mut socket, id.clone(), signing_event()).await;
            if id == json!("first") {
                disconnected(&mut socket).await;
            } else if id == json!(42) {
                socket.close(None).await.unwrap();
            } else {
                // Dropping while a delivery awaits its processing acknowledgement
                // must still cancel the underlying subscription and connection.
                disconnected(&mut socket).await;
            }
        }
    });
    let mut events = EventStream::connect(&url, "tenant-token", CHANNEL_ID).unwrap();
    for processed in [false, true] {
        let delivery = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivery.event, signing_event());
        delivery.processed.send(processed).unwrap();
    }
    let delivery = tokio::time::timeout(Duration::from_secs(5), events.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delivery.event, signing_event());
    drop(events);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn rejected_credentials_stop_without_reconnecting() {
    for status in [401, 403] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.unwrap());
            }
            socket.write_all(format!("HTTP/1.1 {status} Rejected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(600), listener.accept())
                    .await
                    .is_err()
            );
        });
        let mut events = EventStream::connect(&url, "secret-token", CHANNEL_ID).unwrap();
        let error = tokio::time::timeout(Duration::from_secs(2), events.next())
            .await
            .unwrap()
            .err()
            .unwrap();
        assert!(error.to_string().contains("凭证"));
        assert!(!format!("{error:#}").contains("secret-token"));
        assert!(events.next().await.is_err());
        server.await.unwrap();
    }
}

#[tokio::test]
async fn subscription_rejection_stops_and_redacts_the_server_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
        let request: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        socket.send(Message::text(json!({"jsonrpc":"2.0", "id":request["id"],
            "error":{"code":-32001, "message":"Unauthorized secret-token", "data":"secret-payload"}
        }).to_string())).await.unwrap();
        disconnected(&mut socket).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(600), listener.accept())
                .await
                .is_err()
        );
    });
    let mut events = EventStream::connect(&url, "secret-token", CHANNEL_ID).unwrap();
    let error = tokio::time::timeout(Duration::from_secs(2), events.next())
        .await
        .unwrap()
        .err()
        .unwrap();
    assert!(error.to_string().contains("-32001"));
    assert!(!format!("{error:#}").contains("secret-"));
    assert!(events.next().await.is_err());
    server.await.unwrap();
}

#[tokio::test]
async fn dropping_a_silent_subscription_disconnects() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let (connected, ready) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = accept(socket, "tenant-token", CHANNEL_ID, json!(1)).await;
        connected.send(()).unwrap();
        disconnected(&mut socket).await;
    });
    let events = EventStream::connect(&url, "tenant-token", CHANNEL_ID).unwrap();
    tokio::time::timeout(Duration::from_secs(2), ready)
        .await
        .unwrap()
        .unwrap();
    drop(events);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}
