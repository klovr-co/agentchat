//! Socket admission, acknowledgements, reconnection, and owned task lifetime.

use std::future::Future;
use std::sync::{atomic::Ordering, Arc};
use std::time::Duration;

use agentchat_core::{ConversationId, Error, Message, MessageId, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::task::{JoinError, JoinSet};
use tokio::time::{timeout, Instant, MissedTickBehavior};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{protocol::WebSocketConfig, Message as Frame},
    MaybeTlsStream, WebSocketStream,
};

use super::{host::ack_request, Admission, Inner, Slack, CLOSED, DELIVERY_SCOPE, NEW, RUNNING};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct StopOnDrop(Arc<Inner>);
impl Drop for StopOnDrop {
    fn drop(&mut self) {
        Slack {
            inner: self.0.clone(),
        }
        .request_close();
    }
}

struct Finished(Arc<Inner>);
impl Drop for Finished {
    fn drop(&mut self) {
        self.0.connected.store(false, Ordering::Release);
        self.0.lifecycle.store(CLOSED, Ordering::Release);
        self.0.stop.cancel();
        self.0.done.cancel();
    }
}

enum SocketExit {
    Reconnect,
    Shutdown,
}

struct AdmittedMessage {
    inner: Arc<Inner>,
    key: (MessageId, ConversationId),
    id: u64,
    lifetime: tokio_util::sync::CancellationToken,
}

impl AdmittedMessage {
    fn new(inner: Arc<Inner>, message: &Message) -> Result<Self> {
        let key = (message.id.clone(), message.conversation_id.clone());
        let id = inner.next_admission.fetch_add(1, Ordering::Relaxed);
        let lifetime = tokio_util::sync::CancellationToken::new();
        inner
            .admitted
            .lock()
            .map_err(|_| Error::Closed)?
            .entry(key.clone())
            .or_default()
            .push(Admission {
                id,
                source: message.clone(),
                lifetime: lifetime.clone(),
            });
        Ok(Self {
            inner,
            key,
            id,
            lifetime,
        })
    }
}

impl Drop for AdmittedMessage {
    fn drop(&mut self) {
        self.lifetime.cancel();
        let mut admitted = self
            .inner
            .admitted
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(entries) = admitted.get_mut(&self.key) {
            entries.retain(|entry| entry.id != self.id);
            if entries.is_empty() {
                admitted.remove(&self.key);
            }
        }
    }
}

impl Slack {
    pub(super) async fn run_socket<F, Fut>(&self, receiver: F) -> Result<()>
    where
        F: Fn(Message) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        self.inner
            .lifecycle
            .compare_exchange(NEW, RUNNING, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::InvalidInput("Slack has already been started or closed".into()))?;
        let _stop = StopOnDrop(self.inner.clone());
        let slack = self.clone();
        let (send, receive) = oneshot::channel();
        tokio::spawn(async move {
            let _finished = Finished(slack.inner.clone());
            let mut handlers = JoinSet::new();
            let result = slack.drive_socket(Arc::new(receiver), &mut handlers).await;
            slack.inner.connected.store(false, Ordering::Release);
            slack.request_close();
            let drained = timeout(slack.inner.config.shutdown_timeout, async {
                let mut failure = None;
                while let Some(completed) = handlers.join_next().await {
                    if let Err(error) = slack.receiver_result(completed) {
                        if failure.is_none() {
                            failure = Some(error);
                        }
                    }
                }
                failure
            })
            .await;
            let drain_error = match drained {
                Ok(error) => error,
                Err(_) => {
                    handlers.abort_all();
                    while handlers.join_next().await.is_some() {}
                    None
                }
            };
            let result = result.and_then(|()| drain_error.map_or(Ok(()), Err));
            let _ = send.send(result);
        });
        receive
            .await
            .map_err(|_| Error::Transport("Slack transport task stopped unexpectedly".into()))?
    }

    async fn drive_socket<F, Fut>(
        &self,
        receiver: Arc<F>,
        handlers: &mut JoinSet<Result<()>>,
    ) -> Result<()>
    where
        F: Fn(Message) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        tokio::select! {
            biased;
            _ = self.inner.stop.cancelled() => return Ok(()),
            result = self.authenticate() => { result?; }
        }
        let mut has_connected = false;
        let mut retry_seconds = 1;
        loop {
            let open_request = json!({});
            let response = tokio::select! {
                biased;
                _ = self.inner.stop.cancelled() => return Ok(()),
                result = self.inner.client.call("apps.connections.open", &open_request) => result,
            };
            let response = match response {
                Ok(response) => response,
                Err(error) if has_connected && error.retryable_read() => {
                    if self.reconnect_delay(retry_seconds).await {
                        return Ok(());
                    }
                    retry_seconds = (retry_seconds * 2).min(30);
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let url = response["url"]
                .as_str()
                .ok_or_else(|| Error::Transport("Slack did not return a socket URL".into()))?;
            self.inner.client.validate_socket_url(url)?;
            let config = WebSocketConfig::default()
                .max_message_size(Some(2 * 1024 * 1024))
                .max_frame_size(Some(2 * 1024 * 1024));
            let connection = tokio::select! {
                biased;
                _ = self.inner.stop.cancelled() => return Ok(()),
                result = timeout(Duration::from_secs(30), connect_async_with_config(url, Some(config), false)) => result,
            };
            let mut socket = match connection {
                Ok(Ok((socket, _))) => socket,
                _ if has_connected => {
                    if self.reconnect_delay(retry_seconds).await {
                        return Ok(());
                    }
                    retry_seconds = (retry_seconds * 2).min(30);
                    continue;
                }
                _ => {
                    return Err(Error::Transport(
                        "Slack Socket Mode connection failed".into(),
                    ))
                }
            };
            has_connected = true;
            retry_seconds = 1;
            self.inner.connected.store(true, Ordering::Release);
            let outcome = self
                .read_socket(&mut socket, receiver.clone(), handlers)
                .await;
            self.inner.connected.store(false, Ordering::Release);
            // Drop the socket immediately when shutting down; a stalled close handshake must
            // not consume the handler-drain budget. TCP/TLS resources are released on drop.
            drop(socket);
            match outcome? {
                SocketExit::Shutdown => return Ok(()),
                SocketExit::Reconnect => {
                    if self.reconnect_delay(retry_seconds).await {
                        return Ok(());
                    }
                }
            }
        }
    }

    async fn reconnect_delay(&self, seconds: u64) -> bool {
        tokio::select! {
            biased;
            _ = self.inner.stop.cancelled() => true,
            _ = tokio::time::sleep(Duration::from_secs(seconds)) => false,
        }
    }

    async fn read_socket<F, Fut>(
        &self,
        socket: &mut Socket,
        receiver: Arc<F>,
        handlers: &mut JoinSet<Result<()>>,
    ) -> Result<SocketExit>
    where
        F: Fn(Message) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
        heartbeat.tick().await;
        let mut last_received = Instant::now();
        loop {
            let frame = tokio::select! {
                biased;
                _ = self.inner.stop.cancelled() => return Ok(SocketExit::Shutdown),
                completed = handlers.join_next(), if !handlers.is_empty() => {
                    if let Some(completed) = completed { self.receiver_result(completed)?; }
                    continue;
                },
                _ = heartbeat.tick() => {
                    if last_received.elapsed() > Duration::from_secs(45)
                        || !self.send_frame(socket, Frame::Ping(Vec::new().into())).await {
                        return Ok(SocketExit::Reconnect);
                    }
                    continue;
                },
                frame = socket.next() => frame,
            };
            let Some(Ok(frame)) = frame else {
                return Ok(SocketExit::Reconnect);
            };
            last_received = Instant::now();
            let text = match frame {
                Frame::Text(text) => text,
                Frame::Ping(payload) => {
                    if !self.send_frame(socket, Frame::Pong(payload)).await {
                        return Ok(SocketExit::Reconnect);
                    }
                    continue;
                }
                Frame::Close(_) => return Ok(SocketExit::Reconnect),
                _ => continue,
            };
            let Ok(envelope) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if envelope["type"] == "disconnect" {
                if envelope["reason"] == "link_disabled" {
                    return Err(Error::Transport("Slack Socket Mode was disabled".into()));
                }
                return Ok(SocketExit::Reconnect);
            }
            // Events, and interactions such as presses on prompt buttons.
            if envelope["type"] != "events_api" && envelope["type"] != "interactive" {
                continue;
            }
            let Some(envelope_id) = super::events::nonempty(&envelope["envelope_id"]) else {
                continue;
            };
            if !self.inner.accepting.load(Ordering::Acquire)
                || handlers.len() >= self.inner.config.max_pending_events
            {
                // Leave overflow unacknowledged so Slack can apply its retry policy.
                continue;
            }
            if !self
                .send_frame(
                    socket,
                    Frame::Text(json!({"envelope_id":envelope_id}).to_string().into()),
                )
                .await
            {
                return Ok(SocketExit::Reconnect);
            }
            let slack = self.clone();
            let payload = envelope["payload"].clone();
            let receive = receiver.clone();
            let scope = slack.inner.clone();
            handlers.spawn(DELIVERY_SCOPE.scope(scope, async move {
                // Accepted work remains valid during shutdown and is drained before CLOSED.
                if let Some(message) = slack.prepare_event(&payload).await? {
                    let _admitted = AdmittedMessage::new(slack.inner.clone(), &message)?;
                    let press = message
                        .metadata
                        .contains_key(agentchat_core::ANSWER_METADATA);
                    if let Some(emoji) = slack
                        .inner
                        .config
                        .ack_emoji
                        .as_deref()
                        .filter(|v| !v.is_empty() && !press)
                    {
                        let _ = slack
                            .inner
                            .client
                            .call(
                                "reactions.add",
                                &ack_request(
                                    message
                                        .metadata
                                        .get("channel_id")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default(),
                                    emoji,
                                    message
                                        .metadata
                                        .get("message_timestamp")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default(),
                                ),
                            )
                            .await;
                    }
                    receive(message).await?;
                }
                Ok(())
            }));
        }
    }

    async fn send_frame(&self, socket: &mut Socket, frame: Frame) -> bool {
        tokio::select! {
            biased;
            _ = self.inner.stop.cancelled() => false,
            result = timeout(Duration::from_secs(5), socket.send(frame)) => matches!(result, Ok(Ok(()))),
        }
    }

    fn receiver_result(&self, result: std::result::Result<Result<()>, JoinError>) -> Result<()> {
        let result =
            result.unwrap_or_else(|_| Err(Error::Handler("Slack receiver task failed".into())));
        if self.inner.config.stop_on_handler_error {
            return result;
        }
        if result.is_err() {
            tracing::warn!("Slack receiver failed; continuing to accept events");
        }
        Ok(())
    }
}
