//! Bounded WebSocket signing subscriptions with pending-request replay on reconnect.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fiber_json_types::{Hash256, SubscriptionEvent, SubscriptionOptions, SubscriptionTopic};
use jsonrpsee::{
    client_transport::ws::WsHandshakeError,
    core::client::{Error, SubscriptionClientT},
    rpc_params,
    ws_client::{HeaderMap, HeaderValue, PingConfig, WsClientBuilder},
};
use tokio::sync::{mpsc, oneshot};
use zeroize::Zeroizing;

const MAX_EVENT_BYTES: u32 = 10 * 1024 * 1024;
const MAX_BACKOFF: Duration = Duration::from_secs(30);

enum StreamEnd {
    Reconnect,
    TokenRejected,
    SubscriptionRejected(i32),
}

/// One event is acknowledged after verification and signature submission.
pub struct EventDelivery {
    pub event: SubscriptionEvent,
    /// Send false on failure to reconnect and recover outstanding requests.
    pub processed: oneshot::Sender<bool>,
}

/// Each connection has its own subscription. Dropping the reader cancels it,
/// including any connection attempt, pending delivery, or reconnect delay.
pub struct EventStream {
    receiver: mpsc::Receiver<Result<EventDelivery>>,
    task: tokio::task::JoinHandle<()>,
}

pub fn parse_url(value: &str) -> Result<String> {
    let url = reqwest::Url::parse(value).context("LSP WebSocket URL 无效")?;
    anyhow::ensure!(
        matches!(url.scheme(), "ws" | "wss")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "LSP WebSocket URL 必须为不含凭证、查询参数和片段的 WS(S) 地址"
    );
    Ok(url.into())
}

impl EventStream {
    pub(crate) fn connect(ws_url: &str, token: &str, channel_id: Hash256) -> Result<Self> {
        let url = parse_url(ws_url)?;
        let token = Zeroizing::new(token.to_owned());
        let mut auth = HeaderValue::from_str(&format!("Bearer {}", token.as_str()))
            .map_err(|_| anyhow::anyhow!("用户凭证无法用于 WebSocket 请求头"))?;
        auth.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert("authorization", auth);
        let builder = WsClientBuilder::default()
            .set_headers(headers)
            // jsonrpsee counts the initial handshake in this limit.
            .max_redirections(1)
            .connection_timeout(Duration::from_secs(10))
            .request_timeout(Duration::from_secs(10))
            .max_response_size(MAX_EVENT_BYTES)
            .max_buffer_capacity_per_subscription(32)
            .enable_ws_ping(PingConfig::default());
        let (sender, receiver) = mpsc::channel(1);
        let task = tokio::spawn(async move {
            let mut backoff = Duration::from_millis(500);
            loop {
                let started = Instant::now();
                let result = receive(&builder, &url, channel_id, &sender).await;
                if sender.is_closed() {
                    break;
                }
                let terminal = match result {
                    Ok(StreamEnd::TokenRejected) => Some(anyhow::anyhow!(
                        "LSP 拒绝 WebSocket 用户凭证；停止本次收付款推进"
                    )),
                    Ok(StreamEnd::SubscriptionRejected(code)) => Some(anyhow::anyhow!(
                        "LSP 拒绝 WebSocket channel_signing 订阅（code {code}）；请检查独立 WS 地址、通道和用户凭证的 subscriptions / channels 权限；旧租户凭证需由服务端重新签发"
                    )),
                    _ => None,
                };
                if let Some(error) = terminal {
                    let _ = sender.send(Err(error)).await;
                    break;
                }
                // Never include transport errors or signing payloads in diagnostics.
                eprintln!("LSP WebSocket 订阅中断，稍后重连并重新订阅待签名请求");
                if started.elapsed() >= Duration::from_secs(30) {
                    backoff = Duration::from_millis(500);
                }
                let half = (backoff.as_millis() as u64 / 2).max(1);
                let delay = Duration::from_millis(half + rand::random::<u64>() % (half + 1));
                tokio::select! {
                    _ = sender.closed() => break,
                    _ = tokio::time::sleep(delay) => {}
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        });
        Ok(Self { receiver, task })
    }

    pub async fn next(&mut self) -> Result<EventDelivery> {
        self.receiver
            .recv()
            .await
            .context("LSP WebSocket 事件读取已停止")?
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn receive(
    builder: &WsClientBuilder,
    url: &str,
    channel_id: Hash256,
    sender: &mpsc::Sender<Result<EventDelivery>>,
) -> Result<StreamEnd> {
    let client = match builder.clone().build(url).await {
        Ok(client) => client,
        Err(Error::Transport(error))
            if matches!(
                error.downcast_ref::<WsHandshakeError>(),
                Some(WsHandshakeError::Rejected {
                    status_code: 401 | 403
                })
            ) =>
        {
            return Ok(StreamEnd::TokenRejected);
        }
        Err(error) => return Err(error.into()),
    };
    let options = SubscriptionOptions {
        channel_ids: Some(vec![channel_id]),
    };
    let mut subscription = match client
        .subscribe::<SubscriptionEvent, _>(
            "subscribe",
            rpc_params![SubscriptionTopic::ChannelSigning, options],
            "unsubscribe",
        )
        .await
    {
        Ok(subscription) => subscription,
        Err(Error::Call(error)) => return Ok(StreamEnd::SubscriptionRejected(error.code())),
        Err(error) => return Err(error.into()),
    };
    // The server sends pending requests first, then current state changes. There
    // is no session header, historical cursor, or snapshot-complete event.
    while let Some(event) = subscription.next().await {
        let event = event?;
        let (processed, reply) = oneshot::channel();
        sender.send(Ok(EventDelivery { event, processed })).await?;
        if !reply.await.unwrap_or(false) {
            break;
        }
    }
    Ok(StreamEnd::Reconnect)
}

#[cfg(test)]
#[path = "../tests/unit/test_events.rs"]
pub(crate) mod tests;
