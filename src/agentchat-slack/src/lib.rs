//! Native Slack transport shared by Rust and Node applications.
//!
//! Channel methods make one outbound attempt. A successful result includes a
//! provider receipt; a failed write may have an uncertain delivery outcome.
//! Socket acknowledgements accept bounded in-memory work, not a durable queue.

mod apps;
pub mod events;
pub mod file_transfers;
mod files;
pub mod host;
pub mod interaction;
pub mod message_request;
pub mod policy;
pub mod rendering;
mod route;
mod socket;
mod transport;
pub mod web_api;

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
    Arc,
};
use std::time::Duration;

use agentchat_core::{
    Answer, Attachment, Channel, ConversationId, DownloadedFile, Error, Message, MessageId, Prompt,
    Result, RichReply, StreamHandle, StreamOptions, StreamUpdate, UploadFile, UploadedFile,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

pub use apps::{AppHandler, Matcher};
pub use interaction::{Route, ViewTarget, ACK_DEADLINE};
pub use regex::Regex;
use route::Destination;
use transport::{RequestError, WebClient};

const NEW: u8 = 0;
const RUNNING: u8 = 1;
const CLOSING: u8 = 2;
const CLOSED: u8 = 3;

tokio::task_local! { static DELIVERY_SCOPE: Arc<Inner>; }

#[derive(Clone)]
pub struct SlackConfig {
    pub bot_token: String,
    pub app_token: String,
    pub workspace_id: Option<String>,
    pub bot_user_id: Option<String>,
    pub ack_emoji: Option<String>,
    pub max_pending_events: usize,
    pub shutdown_timeout: Duration,
    /// Preserve the Python adapter's continue-on-handler-error behavior by default.
    pub stop_on_handler_error: bool,
    /// Override for a trusted API proxy or loopback test server. Never agent-controlled.
    pub api_base_url: String,
    /// Report working status through Slack agent sessions
    /// (`agents.sessions.setStatus`) instead of `assistant.threads.setStatus`.
    /// Needs an app declared as an agent; off by default.
    pub agent_sessions: bool,
}

impl SlackConfig {
    pub fn new(bot_token: String, app_token: String) -> Self {
        Self {
            bot_token,
            app_token,
            workspace_id: None,
            bot_user_id: None,
            ack_emoji: Some("eyes".into()),
            max_pending_events: 64,
            shutdown_timeout: Duration::from_secs(20),
            stop_on_handler_error: false,
            api_base_url: "https://slack.com/api/".into(),
            agent_sessions: false,
        }
    }

    pub fn from_env() -> Result<Self> {
        let bot_token = std::env::var("SLACK_BOT_TOKEN")
            .map_err(|_| Error::InvalidInput("SLACK_BOT_TOKEN is required".into()))?;
        let app_token = std::env::var("SLACK_APP_TOKEN")
            .map_err(|_| Error::InvalidInput("SLACK_APP_TOKEN is required".into()))?;
        Ok(Self::new(bot_token, app_token))
    }
}

impl fmt::Debug for SlackConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlackConfig")
            .field("bot_token", &"[REDACTED]")
            .field("app_token", &"[REDACTED]")
            .field("workspace_id", &self.workspace_id)
            .field("max_pending_events", &self.max_pending_events)
            .field("agent_sessions", &self.agent_sessions)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SlackIdentity {
    pub workspace_id: String,
    pub bot_user_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SlackUser {
    pub id: String,
    pub team_id: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub is_bot: bool,
    pub deleted: bool,
}

/// Applications may persist or expire subscriptions in their own store.
#[async_trait]
pub trait ThreadSubscriptions: Send + Sync {
    async fn contains(&self, conversation_id: &str) -> Result<bool>;
    async fn add(&self, conversation_id: &str) -> Result<()>;
}

#[derive(Default)]
pub struct MemoryThreadSubscriptions {
    threads: Mutex<policy::SubscriptionCache>,
}

#[async_trait]
impl ThreadSubscriptions for MemoryThreadSubscriptions {
    async fn contains(&self, conversation_id: &str) -> Result<bool> {
        Ok(self.threads.lock().await.contains(conversation_id))
    }
    async fn add(&self, conversation_id: &str) -> Result<()> {
        self.threads.lock().await.add(conversation_id);
        Ok(())
    }
}

/// What an authenticated payload is for: a message for the runtime (a chat
/// message, prompt answer or stop request), the application's handlers, or both.
#[derive(Debug, Default)]
pub struct Inbound {
    pub message: Option<Message>,
    pub application: bool,
}

/// The result of host ingress: the message for the runtime, if any, and the
/// acknowledgement body to return to Slack, if any.
#[derive(Debug, Default)]
pub struct Handled {
    pub message: Option<Message>,
    pub ack: Option<Value>,
}

struct Admission {
    id: u64,
    source: Message,
    lifetime: CancellationToken,
}

struct Inner {
    config: SlackConfig,
    client: WebClient,
    identity: RwLock<Option<SlackIdentity>>,
    authentication: Mutex<()>,
    subscriptions: Arc<dyn ThreadSubscriptions>,
    lifecycle: AtomicU8,
    connected: AtomicBool,
    accepting: AtomicBool,
    stop: CancellationToken,
    done: CancellationToken,
    admitted: std::sync::Mutex<HashMap<(MessageId, ConversationId), Vec<Admission>>>,
    next_admission: AtomicU64,
    routes: std::sync::RwLock<apps::Routes>,
}

#[derive(Clone)]
pub struct Slack {
    inner: Arc<Inner>,
}

impl Slack {
    pub fn new(config: SlackConfig) -> Result<Self> {
        Self::with_thread_subscriptions(config, Arc::new(MemoryThreadSubscriptions::default()))
    }

    pub fn from_env() -> Result<Self> {
        Self::new(SlackConfig::from_env()?)
    }

    pub fn with_thread_subscriptions(
        config: SlackConfig,
        subscriptions: Arc<dyn ThreadSubscriptions>,
    ) -> Result<Self> {
        if config.bot_token.trim().is_empty() || config.app_token.trim().is_empty() {
            return Err(Error::InvalidInput(
                "Slack bot_token and app_token are required".into(),
            ));
        }
        if config.max_pending_events == 0 {
            return Err(Error::InvalidInput(
                "max_pending_events must be positive".into(),
            ));
        }
        let client = WebClient::new(
            &config.api_base_url,
            config.bot_token.clone(),
            config.app_token.clone(),
        )?;
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                client,
                identity: RwLock::new(None),
                authentication: Mutex::new(()),
                subscriptions,
                lifecycle: AtomicU8::new(NEW),
                connected: AtomicBool::new(false),
                accepting: AtomicBool::new(true),
                stop: CancellationToken::new(),
                done: CancellationToken::new(),
                admitted: std::sync::Mutex::new(HashMap::new()),
                next_admission: AtomicU64::new(0),
                routes: std::sync::RwLock::new(apps::Routes::default()),
            }),
        })
    }

    /// Authenticate before accepting normalized events, including with host-owned ingress.
    pub async fn authenticate(&self) -> Result<SlackIdentity> {
        self.check_open()?;
        let _guard = self.inner.authentication.lock().await;
        if let Some(identity) = self.inner.identity.read().await.clone() {
            return Ok(identity);
        }
        let response = self.inner.client.call("auth.test", &json!({})).await?;
        let workspace_id = events::nonempty(&response["team_id"])
            .ok_or_else(|| Error::Transport("Slack did not return a workspace identity".into()))?;
        let bot_user_id = events::nonempty(&response["user_id"])
            .ok_or_else(|| Error::Transport("Slack did not return a bot identity".into()))?;
        if self
            .inner
            .config
            .workspace_id
            .as_ref()
            .is_some_and(|id| id != workspace_id)
            || self
                .inner
                .config
                .bot_user_id
                .as_ref()
                .is_some_and(|id| id != bot_user_id)
        {
            return Err(Error::InvalidInput(
                "Slack token belongs to a different workspace or bot".into(),
            ));
        }
        self.check_open()?;
        let identity = SlackIdentity {
            workspace_id: workspace_id.into(),
            bot_user_id: bot_user_id.into(),
        };
        *self.inner.identity.write().await = Some(identity.clone());
        Ok(identity)
    }

    pub fn is_connected(&self) -> bool {
        self.inner.connected.load(Ordering::Acquire) && !self.inner.stop.is_cancelled()
    }

    fn check_open(&self) -> Result<()> {
        let state = self.inner.lifecycle.load(Ordering::Acquire);
        if state == CLOSED || (state == CLOSING && !self.in_delivery()) {
            Err(Error::Closed)
        } else {
            Ok(())
        }
    }

    fn in_delivery(&self) -> bool {
        DELIVERY_SCOPE
            .try_with(|inner| Arc::ptr_eq(inner, &self.inner))
            .unwrap_or(false)
    }

    /// Restore an admitted receiver's operation scope across an FFI task boundary.
    /// Outside a live receiver, normal lifecycle checks still apply.
    pub async fn with_admitted_source<T, F>(&self, source: &Message, operation: F) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        let lifetime = self
            .inner
            .admitted
            .lock()
            .map_err(|_| Error::Closed)?
            .get(&(source.id.clone(), source.conversation_id.clone()))
            .and_then(|entries| entries.iter().find(|entry| entry.source == *source))
            .map(|entry| entry.lifetime.clone());
        if let Some(lifetime) = lifetime {
            DELIVERY_SCOPE.scope(self.inner.clone(), async {
                tokio::select! {
                    biased;
                    _ = lifetime.cancelled() => Err(Error::Transport("Slack operation was cancelled; delivery may be uncertain".into())),
                    result = operation => result,
                }
            }).await
        } else {
            self.check_open()?;
            operation.await
        }
    }

    async fn identity(&self) -> Result<SlackIdentity> {
        self.check_open()?;
        self.inner.identity.read().await.clone().ok_or_else(|| {
            Error::InvalidInput("Authenticate Slack before using this operation".into())
        })
    }

    async fn destination(&self, source: &Message) -> Result<Destination> {
        Destination::from_message(source, &self.identity().await?.workspace_id)
    }

    /// The caller must authenticate HTTP requests before using this host-ingress hook.
    /// Returns the message for the runtime; see [`Self::handle_payload`] to also run
    /// the application's handlers.
    pub async fn handle_event(&self, payload: &Value) -> Result<Option<Message>> {
        self.check_accepting()?;
        Ok(self.classify(payload).await?.message)
    }

    /// Host ingress for any authenticated payload. Runs the application's handler and
    /// returns the message for the runtime, if any, with the acknowledgement body.
    /// For payloads whose acknowledgement can carry a body (see
    /// [`interaction::responds`]) it waits at most [`ACK_DEADLINE`]; a slower handler
    /// keeps running and Slack gets an empty acknowledgement. Acknowledge other
    /// payloads before calling this: it waits for their handler.
    pub async fn handle_payload(&self, payload: &Value) -> Result<Handled> {
        self.check_accepting()?;
        let inbound = self.classify(payload).await?;
        let mut handled = Handled {
            message: inbound.message,
            ack: None,
        };
        if !inbound.application {
            return Ok(handled);
        }
        if interaction::responds(payload) {
            let slack = self.clone();
            let payload = payload.clone();
            let mut task = tokio::spawn(async move { slack.dispatch(payload).await });
            if let Ok(result) = tokio::time::timeout(ACK_DEADLINE, &mut task).await {
                handled.ack =
                    result.map_err(|_| Error::Handler("Slack handler failed".into()))??;
            }
        } else {
            self.dispatch(payload.clone()).await?;
        }
        Ok(handled)
    }

    fn check_accepting(&self) -> Result<()> {
        if self.inner.stop.is_cancelled() || !self.inner.accepting.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        Ok(())
    }

    /// Decide what an authenticated payload is for. Payloads from another workspace
    /// are ignored. Events that do not become a message go to the application.
    pub(crate) async fn classify(&self, payload: &Value) -> Result<Inbound> {
        let identity = self.identity().await?;
        if !interaction::in_workspace(payload, Some(&identity.workspace_id)) {
            return Ok(Inbound::default());
        }
        if payload.get("event").is_some_and(Value::is_object) {
            // Slack's native stop is a stop request, and the application sees it too.
            if let Some(stop) =
                interaction::normalize_stop(payload, Some(&identity.workspace_id), "slack")
            {
                return Ok(Inbound {
                    message: Some(stop),
                    application: true,
                });
            }
            let message =
                match events::normalize(payload, &identity.bot_user_id, &identity.workspace_id) {
                    Some(message)
                        if message.metadata.get("requires_subscription")
                            == Some(&Value::Bool(true))
                            && !self
                                .inner
                                .subscriptions
                                .contains(message.conversation_id.as_ref())
                                .await? =>
                    {
                        None
                    }
                    message => message,
                };
            return Ok(Inbound {
                application: message.is_none(),
                message,
            });
        }
        // A press on a prompt button answers that prompt; other presses are the app's.
        if payload["type"] == "block_actions" {
            if let Some(press) = events::normalize_action(
                payload,
                Some(&identity.bot_user_id),
                Some(&identity.workspace_id),
                "slack",
            ) {
                return Ok(Inbound {
                    message: Some(press),
                    application: false,
                });
            }
        }
        Ok(Inbound {
            message: None,
            application: true,
        })
    }

    /// Run the application's handler for `payload`, if one is registered.
    pub(crate) async fn dispatch(&self, payload: Value) -> Result<Option<Value>> {
        let route = interaction::route(&payload);
        let handler = self
            .inner
            .routes
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .find(&route);
        match handler {
            Some(handler) => handler(payload).await,
            None => Ok(None),
        }
    }

    fn routes(&self) -> std::sync::RwLockWriteGuard<'_, apps::Routes> {
        self.inner
            .routes
            .write()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// Handle Events API events of `event_type` that AgentChat does not turn into
    /// a message, such as `app_home_opened` or `member_joined_channel`. The handler
    /// receives the whole event envelope; the event is `payload["event"]`.
    pub fn on_event<F, Fut>(&self, event_type: impl Into<String>, handler: F)
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Option<Value>>> + Send + 'static,
    {
        self.routes()
            .event(event_type.into(), apps::handler(handler));
    }

    /// Handle button presses and other `block_actions` whose `action_id` matches.
    /// AgentChat's own prompt buttons never reach it.
    pub fn on_action<F, Fut>(&self, action_id: impl Into<Matcher>, handler: F)
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Option<Value>>> + Send + 'static,
    {
        self.routes()
            .action(action_id.into(), apps::handler(handler));
    }

    /// Handle `view_submission` and `view_closed` for views whose `callback_id`
    /// matches. A submission handler's return value, such as
    /// `{"response_action": "errors", "errors": {...}}`, is the acknowledgement body.
    pub fn on_view<F, Fut>(&self, callback_id: impl Into<Matcher>, handler: F)
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Option<Value>>> + Send + 'static,
    {
        self.routes()
            .view(callback_id.into(), apps::handler(handler));
    }

    /// Handle a slash command, such as `/deploy`; the return value is the response.
    pub fn on_command<F, Fut>(&self, command: impl Into<String>, handler: F)
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Option<Value>>> + Send + 'static,
    {
        self.routes()
            .command(command.into(), apps::handler(handler));
    }

    /// Handle every other payload that no handler above matched, for example
    /// shortcuts and options requests. Without handlers, payloads are acknowledged
    /// and ignored.
    pub fn on_raw<F, Fut>(&self, handler: F)
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Option<Value>>> + Send + 'static,
    {
        self.routes().raw(apps::handler(handler));
    }

    /// Remove every registered handler, for example when a foreign-language host
    /// that registered them stops.
    pub fn clear_handlers(&self) {
        self.routes().clear();
    }

    /// After Slack's native stop, move its agent session out of `processing`; Slack
    /// leaves that to the app. Best effort: the stop itself already happened.
    pub(crate) async fn after_stop(&self, stop: &Message) {
        if self.set_status(stop, "").await.is_err() {
            tracing::warn!("Could not clear the Slack status after a stop");
        }
    }

    /// Subscribe only after the application authorizes the sender and conversation.
    pub async fn subscribe(&self, source: &Message) -> Result<()> {
        self.destination(source).await?;
        if source.metadata.get("channel_type") != Some(&Value::String("im".into())) {
            self.inner
                .subscriptions
                .add(source.conversation_id.as_ref())
                .await?;
        }
        Ok(())
    }

    /// Run Socket Mode until closed. Dropping this future requests bounded cleanup.
    pub async fn run<F, Fut>(&self, receiver: F) -> Result<()>
    where
        F: Fn(Message) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        self.run_socket(receiver).await
    }

    /// Stop intake immediately without waiting for the calling receiver to finish.
    pub fn request_close(&self) {
        self.stop_admission();
        loop {
            match self.inner.lifecycle.load(Ordering::Acquire) {
                NEW => {
                    if self
                        .inner
                        .lifecycle
                        .compare_exchange(NEW, CLOSED, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        self.inner.stop.cancel();
                        self.inner.done.cancel();
                        return;
                    }
                }
                RUNNING => {
                    if self
                        .inner
                        .lifecycle
                        .compare_exchange(RUNNING, CLOSING, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        self.inner.stop.cancel();
                        break;
                    }
                }
                CLOSING => break,
                _ => return,
            }
        }
    }

    /// Stop accepting new events while existing operations finish. Hosts may clear
    /// working indicators here before calling `close()` to release the transport.
    pub fn stop_admission(&self) {
        self.inner.accepting.store(false, Ordering::Release);
    }

    pub async fn close(&self) -> Result<()> {
        self.request_close();
        if self.in_delivery() {
            return Ok(());
        }
        self.inner.done.cancelled().await;
        Ok(())
    }

    fn sent_message(
        source: &Message,
        destination: &Destination,
        content: String,
        receipt: policy::MessageReceipt,
    ) -> Message {
        policy::reply_message(
            source,
            policy::ReplyMessageRoute {
                channel_id: destination.channel_id.clone(),
                thread_timestamp: json!(destination.thread_timestamp),
                team_id: Some(destination.team_id.clone()),
            },
            content,
            receipt,
            None,
        )
    }

    async fn post_message(
        &self,
        source: &Message,
        destination: &Destination,
        mut payload: Value,
        content: &str,
    ) -> Result<Message> {
        payload["channel"] = Value::String(destination.channel_id.clone());
        if let Some(thread) = &destination.thread_timestamp {
            payload["thread_ts"] = Value::String(thread.to_string());
        }
        let response = self.inner.client.call("chat.postMessage", &payload).await?;
        let receipt =
            policy::MessageReceipt::confirmed_timestamp(&response, policy::DeliveryKind::Reply)?;
        Ok(Self::sent_message(
            source,
            destination,
            content.into(),
            receipt,
        ))
    }

    pub async fn reply(&self, source: &Message, content: &str) -> Result<Message> {
        let destination = self.destination(source).await?;
        let blocks = rendering::text_blocks(content)?;
        self.post_message(
            source,
            &destination,
            json!({"text":content,"blocks":blocks,
            "unfurl_links":false,"unfurl_media":false}),
            content,
        )
        .await
    }

    pub async fn reply_rich(&self, source: &Message, content: &RichReply) -> Result<Message> {
        let destination = self.destination(source).await?;
        self.post_message(
            source,
            &destination,
            rendering::rich_payload(content)?,
            &content.text,
        )
        .await
    }

    pub async fn set_status(&self, source: &Message, status: &str) -> Result<bool> {
        let destination = self.destination(source).await?;
        let Some(thread) = destination.thread_timestamp else {
            return Ok(false);
        };
        let (method, request) = policy::status_request(
            &destination.channel_id,
            thread.as_str(),
            status,
            self.inner.config.agent_sessions,
        );
        self.inner.client.call(method, &request).await?;
        Ok(true)
    }

    /// Post a notice in the source conversation that only `user_id` can see. It is
    /// never posted publicly instead.
    pub async fn send_ephemeral(
        &self,
        source: &Message,
        user_id: &str,
        content: &RichReply,
    ) -> Result<Message> {
        let destination = self.destination(source).await?;
        let thread = destination.thread_timestamp.as_ref().map(|ts| ts.as_str());
        let request =
            interaction::ephemeral_request(&destination.channel_id, thread, user_id, content)?;
        let response = self
            .inner
            .client
            .call("chat.postEphemeral", &request)
            .await?;
        interaction::ephemeral_message(
            source,
            &destination.channel_id,
            thread,
            user_id,
            &content.text,
            &response,
        )
    }

    /// Delete a message this app sent (`chat.delete`).
    pub async fn delete(&self, message: &Message) -> Result<()> {
        let identity = self.identity().await?;
        let request = interaction::delete_request(message, Some(&identity.workspace_id))?;
        self.inner.client.call("chat.delete", &request).await?;
        Ok(())
    }

    async fn view_call(&self, method: &str, request: Value) -> Result<Value> {
        self.check_open()?;
        let response = self.inner.client.call(method, &request).await?;
        interaction::view_of(&response)
    }

    /// Open a modal with the `trigger_id` of an interaction, which expires three
    /// seconds after Slack sends it. Returns the opened view.
    pub async fn open_view(&self, trigger_id: &str, view: &Value) -> Result<Value> {
        self.view_call(
            "views.open",
            interaction::open_view_request(trigger_id, view)?,
        )
        .await
    }

    /// Push a view onto an open modal, with the `trigger_id` of an interaction in it.
    pub async fn push_view(&self, trigger_id: &str, view: &Value) -> Result<Value> {
        self.view_call(
            "views.push",
            interaction::open_view_request(trigger_id, view)?,
        )
        .await
    }

    /// Replace a view; `hash` (from the view's payload) guards against races.
    pub async fn update_view(
        &self,
        view: &Value,
        target: &ViewTarget,
        hash: Option<&str>,
    ) -> Result<Value> {
        self.view_call(
            "views.update",
            interaction::update_view_request(view, target, hash)?,
        )
        .await
    }

    /// Publish a user's App Home tab.
    pub async fn publish_home(
        &self,
        user_id: &str,
        view: &Value,
        hash: Option<&str>,
    ) -> Result<Value> {
        self.view_call(
            "views.publish",
            interaction::publish_home_request(user_id, view, hash)?,
        )
        .await
    }

    /// Start a native stream in the source thread. Outside a thread the answer is
    /// posted once when the stream stops.
    pub async fn start_stream(&self, source: &Message, text: &str) -> Result<StreamHandle> {
        let destination = self.destination(source).await?;
        let Some(thread) = destination.thread_timestamp else {
            return Ok(json!({"post": true}));
        };
        let request =
            policy::stream_start_request(&destination.channel_id, thread.as_str(), text, source);
        let response = self.inner.client.call("chat.startStream", &request).await?;
        policy::MessageReceipt::confirmed_timestamp(&response, policy::DeliveryKind::Stream)?;
        Ok(json!({"channel": destination.channel_id, "ts": response["ts"], "sent": text}))
    }

    /// Show task and plan updates. A stream started with them uses Slack's chunks,
    /// so its text can be followed by more updates. If Slack refuses the chunks, the
    /// answer streams as plain text without them.
    pub async fn update_stream_tasks(
        &self,
        source: &Message,
        stream: Option<StreamHandle>,
        updates: &[StreamUpdate],
    ) -> Result<StreamHandle> {
        let destination = self.destination(source).await?;
        let Some(stream) = stream else {
            let Some(thread) = destination.thread_timestamp else {
                return Ok(json!({"post": true}));
            };
            let request = policy::stream_start_tasks_request(
                &destination.channel_id,
                thread.as_str(),
                source,
                updates,
            );
            return match self.inner.client.call("chat.startStream", &request).await {
                Ok(response) => {
                    policy::MessageReceipt::confirmed_timestamp(
                        &response,
                        policy::DeliveryKind::Stream,
                    )?;
                    Ok(policy::chunk_stream_handle(
                        &destination.channel_id,
                        &response["ts"],
                    ))
                }
                // Nothing was started; the answer starts as plain text with its first text.
                Err(RequestError::Api(code)) if policy::retry_without_chunks(&code) => {
                    tracing::warn!("Slack refused task updates; streaming plain text");
                    Ok(json!({"defer": true}))
                }
                Err(error) => Err(error.into()),
            };
        };
        let Some(request) = policy::stream_tasks_request(&stream, updates) else {
            return Ok(stream);
        };
        // Task updates are secondary: a refused update never fails the answer.
        match self.inner.client.call("chat.appendStream", &request).await {
            Ok(_) => Ok(stream),
            Err(_) => {
                tracing::warn!("Slack refused task updates; the answer continues without them");
                Ok(policy::without_tasks(&stream))
            }
        }
    }

    pub async fn append_stream(
        &self,
        source: &Message,
        stream: StreamHandle,
        text: &str,
    ) -> Result<StreamHandle> {
        self.destination(source).await?;
        if stream["post"] == true {
            return Ok(stream);
        }
        if stream["defer"] == true {
            if text.is_empty() {
                return Ok(stream);
            }
            return self.start_stream(source, text).await;
        }
        let (request, next) = policy::stream_request(&stream, text, false)?;
        if let Some(request) = request {
            self.inner
                .client
                .call("chat.appendStream", &request)
                .await?;
        }
        Ok(next)
    }

    pub async fn stop_stream(
        &self,
        source: &Message,
        stream: StreamHandle,
        text: &str,
    ) -> Result<Message> {
        if stream["post"] == true || stream["defer"] == true {
            return self.reply(source, text).await;
        }
        let destination = self.destination(source).await?;
        let (request, _) = policy::stream_request(&stream, text, true)?;
        if let Some(request) = request {
            self.inner.client.call("chat.stopStream", &request).await?;
        }
        let receipt = policy::MessageReceipt::confirmed_timestamp(
            &json!({"ok": true, "ts": stream["ts"]}),
            policy::DeliveryKind::Stream,
        )?;
        Ok(Self::sent_message(
            source,
            &destination,
            text.into(),
            receipt,
        ))
    }

    /// Show a prompt in the source thread: its text and one button per choice.
    pub async fn send_prompt(&self, source: &Message, prompt: &Prompt) -> Result<Message> {
        let destination = self.destination(source).await?;
        let choices: Vec<_> = prompt
            .choices
            .iter()
            .map(|c| (c.id.clone(), c.label.clone(), c.style.as_str().to_owned()))
            .collect();
        let blocks = rendering::prompt_blocks(&prompt.text, &prompt.id, &choices)?;
        self.post_message(
            source,
            &destination,
            json!({"text": prompt.text, "blocks": blocks,
            "unfurl_links": false, "unfurl_media": false}),
            &prompt.text,
        )
        .await
    }

    /// Replace the buttons with the choice and who made it, or remove them on expiry.
    pub async fn close_prompt(
        &self,
        source: &Message,
        sent: &Message,
        prompt: &Prompt,
        answer: Option<&Answer>,
    ) -> Result<()> {
        let destination = self.destination(source).await?;
        let text = answer.map_or_else(|| prompt.text.clone(), |a| a.closed_text(prompt));
        let request = json!({
            "channel": destination.channel_id,
            "ts": sent.metadata.get("message_timestamp"),
            "text": text,
            "blocks": rendering::text_blocks(&text)?,
        });
        self.inner.client.call("chat.update", &request).await?;
        Ok(())
    }

    pub async fn mirror(
        &self,
        source: &Message,
        message: &Message,
        origin: &str,
    ) -> Result<Message> {
        let destination = self.destination(source).await?;
        let mut sent = self
            .post_message(
                source,
                &destination,
                rendering::mirror_payload(message, origin)?,
                &message.text,
            )
            .await?;
        policy::MirroredReply { message, origin }.apply(&mut sent);
        Ok(sent)
    }

    pub async fn get_user(&self, user_id: &str) -> Result<SlackUser> {
        self.identity().await?;
        let response = self
            .inner
            .client
            .get("users.info", &json!({"user": user_id}))
            .await?;
        policy::user_profile(&response, user_id)
    }

    pub async fn thread_history(&self, source: &Message, limit: usize) -> Result<Vec<Message>> {
        self.destination(source).await?;
        let identity = self.identity().await?;
        let mut history = policy::ThreadHistory::new(
            source.clone(),
            Some(identity.bot_user_id),
            Some(&identity.workspace_id),
            limit,
        )?;
        if history.is_root() {
            return Ok(Vec::new());
        }
        loop {
            let response = self
                .inner
                .client
                .get("conversations.replies", &history.request())
                .await?;
            if history.accept(&response)? {
                return Ok(history.messages());
            }
        }
    }
}

#[async_trait]
impl Channel for Slack {
    fn name(&self) -> &str {
        "slack"
    }
    async fn reply(&self, source: &Message, content: &str) -> Result<Message> {
        Slack::reply(self, source, content).await
    }
    async fn close(&self) -> Result<()> {
        Slack::close(self).await
    }
    async fn thread_history(&self, source: &Message, limit: usize) -> Result<Option<Vec<Message>>> {
        Slack::thread_history(self, source, limit).await.map(Some)
    }
    async fn reply_rich(&self, source: &Message, content: &RichReply) -> Result<Message> {
        Slack::reply_rich(self, source, content).await
    }
    async fn set_status(&self, source: &Message, status: &str) -> Result<bool> {
        Slack::set_status(self, source, status).await
    }
    async fn mirror(&self, source: &Message, message: &Message, origin: &str) -> Result<Message> {
        Slack::mirror(self, source, message, origin).await
    }
    /// Slack accepts a few stream updates per second; one a second leaves headroom.
    fn streaming(&self) -> Option<StreamOptions> {
        Some(StreamOptions {
            limit: Some(policy::STREAM_LIMIT),
            ..StreamOptions::default()
        })
    }
    async fn start_stream(&self, source: &Message, text: &str) -> Result<StreamHandle> {
        Slack::start_stream(self, source, text).await
    }
    async fn append_stream(
        &self,
        source: &Message,
        stream: StreamHandle,
        text: &str,
    ) -> Result<StreamHandle> {
        Slack::append_stream(self, source, stream, text).await
    }
    async fn stop_stream(
        &self,
        source: &Message,
        stream: StreamHandle,
        text: &str,
    ) -> Result<Message> {
        Slack::stop_stream(self, source, stream, text).await
    }
    fn stream_tasks(&self) -> bool {
        true
    }
    async fn update_stream_tasks(
        &self,
        source: &Message,
        stream: Option<StreamHandle>,
        updates: &[StreamUpdate],
    ) -> Result<StreamHandle> {
        Slack::update_stream_tasks(self, source, stream, updates).await
    }
    async fn send_ephemeral(
        &self,
        source: &Message,
        user_id: &str,
        content: &RichReply,
    ) -> Result<Message> {
        Slack::send_ephemeral(self, source, user_id, content).await
    }
    async fn delete(&self, message: &Message) -> Result<()> {
        Slack::delete(self, message).await
    }
    fn supports_prompts(&self) -> bool {
        true
    }
    async fn send_prompt(&self, source: &Message, prompt: &Prompt) -> Result<Message> {
        Slack::send_prompt(self, source, prompt).await
    }
    async fn close_prompt(
        &self,
        source: &Message,
        sent: &Message,
        prompt: &Prompt,
        answer: Option<&Answer>,
    ) -> Result<()> {
        Slack::close_prompt(self, source, sent, prompt, answer).await
    }
    async fn upload_files(
        &self,
        source: &Message,
        files: &[UploadFile],
    ) -> Result<Vec<UploadedFile>> {
        Slack::upload_files(self, source, files).await
    }
    async fn download_attachment(
        &self,
        source: &Message,
        attachment: &Attachment,
        max_bytes: usize,
    ) -> Result<DownloadedFile> {
        Slack::download_attachment(self, source, attachment, max_bytes).await
    }
}
