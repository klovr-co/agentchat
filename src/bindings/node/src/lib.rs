use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use agentchat_core::{
    Allowed, AnswerStream, AskOptions, Attachment, Busy, Channel, Choice, ConversationId,
    Error as CoreError, Message, RichReply, Runtime, StopOptions, TaskStatus, TaskUpdate,
    TurnControl, UploadFile,
};
use agentchat_slack::{interaction, Slack, SlackConfig, ViewTarget};
use napi::bindgen_prelude::*;
use napi::threadsafe_function::ThreadsafeFunction;
use napi_derive::napi;
use serde::Deserialize;
use tokio::sync::Mutex;
use tokio::task::JoinSet;

type Handler = ThreadsafeFunction<
    FnArgs<(u32, String, u32)>,
    Promise<Option<String>>,
    FnArgs<(u32, String, u32)>,
    Status,
    false,
>;

/// The application's Slack handlers: `(channelIndex, {"route", "payload"} JSON)`,
/// resolving to the acknowledgement body as JSON, or null.
type AppHandler = ThreadsafeFunction<
    FnArgs<(u32, String)>,
    Promise<Option<String>>,
    FnArgs<(u32, String)>,
    Status,
    false,
>;

/// Running handlers' turn controls, by the turn ID passed to the JavaScript handler.
#[derive(Default)]
struct Turns {
    next: AtomicU32,
    controls: std::sync::Mutex<HashMap<u32, TurnControl>>,
}

impl Turns {
    fn controls(&self) -> std::sync::MutexGuard<'_, HashMap<u32, TurnControl>> {
        self.controls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
    fn get(&self, id: u32) -> Option<TurnControl> {
        self.controls().get(&id).cloned()
    }
}

/// Forgets a turn's control once its JavaScript handler settles.
struct TurnEntry<'a> {
    turns: &'a Turns,
    id: u32,
}

impl Drop for TurnEntry<'_> {
    fn drop(&mut self) {
        self.turns.controls().remove(&self.id);
    }
}

fn allowed(value: Option<String>) -> napi::Result<Allowed> {
    match value.as_deref() {
        None => Ok(Allowed::Sender),
        Some(json) => {
            match serde_json::from_str::<serde_json::Value>(json).map_err(native_error)? {
                serde_json::Value::Null => Ok(Allowed::Sender),
                serde_json::Value::String(anyone) if anyone == "anyone" => Ok(Allowed::Anyone),
                ids @ serde_json::Value::Array(_) => Ok(Allowed::Senders(
                    serde_json::from_value(ids).map_err(native_error)?,
                )),
                _ => Err(native_error(
                    "allowed must be \"anyone\" or a list of sender IDs",
                )),
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SlackOptions {
    bot_token: String,
    app_token: String,
    workspace_id: Option<String>,
    bot_user_id: Option<String>,
    #[serde(default = "default_ack_emoji")]
    ack_emoji: Option<String>,
    max_pending_events: Option<usize>,
    shutdown_timeout_ms: Option<u64>,
    api_base_url: Option<String>,
    #[serde(default)]
    agent_sessions: bool,
}

fn default_ack_emoji() -> Option<String> {
    Some("eyes".into())
}

#[napi(object)]
pub struct NativeUploadFile {
    pub filename: String,
    pub content: Buffer,
    pub title: Option<String>,
}

#[napi(object)]
pub struct NativeDownloadedFile {
    pub id: String,
    pub name: String,
    pub content: Buffer,
    pub media_type: Option<String>,
}

#[napi]
pub struct NativeWorkingStatus {
    inner: Arc<Mutex<agentchat_core::WorkingStatus>>,
}

#[napi]
impl NativeWorkingStatus {
    #[napi]
    pub async fn update(&self, status: String) -> bool {
        self.inner.lock().await.update(&status).await
    }

    #[napi]
    pub async fn close(&self) {
        self.inner.lock().await.close().await;
    }
}

fn native_error(error: impl std::fmt::Display) -> napi::Error {
    napi::Error::from_reason(error.to_string())
}

fn decode_message(value: &str) -> napi::Result<Message> {
    serde_json::from_str(value).map_err(native_error)
}

fn encode_message(value: &Message) -> napi::Result<String> {
    serde_json::to_string(value).map_err(native_error)
}

#[napi]
pub struct NativeAnswerStream {
    slack: Arc<Slack>,
    source: Message,
    inner: Arc<Mutex<AnswerStream>>,
}

#[napi]
impl NativeAnswerStream {
    #[napi]
    pub async fn append(&self, text: String) -> napi::Result<()> {
        let mut stream = self.inner.lock().await;
        self.slack
            .with_admitted_source(&self.source, stream.append(&text))
            .await
            .map_err(native_error)
    }

    /// Returns the confirmed messages as JSON.
    #[napi]
    pub async fn finish(&self) -> napi::Result<String> {
        let mut stream = self.inner.lock().await;
        self.slack
            .with_admitted_source(&self.source, stream.finish())
            .await
            .map_err(native_error)?;
        serde_json::to_string(stream.messages()).map_err(native_error)
    }

    #[napi]
    pub async fn abort(&self) -> napi::Result<()> {
        let mut stream = self.inner.lock().await;
        self.slack
            .with_admitted_source(&self.source, stream.abort())
            .await
            .map_err(native_error)
    }

    /// Show a task's progress with the answer: status is "pending", "in_progress",
    /// "complete" or "error".
    #[napi]
    pub async fn task(
        &self,
        id: String,
        title: String,
        status: String,
        details: Option<String>,
        output: Option<String>,
    ) -> napi::Result<()> {
        let status: TaskStatus = status.parse().map_err(native_error)?;
        let update = TaskUpdate {
            id,
            title,
            status,
            details,
            output,
        };
        let mut stream = self.inner.lock().await;
        self.slack
            .with_admitted_source(&self.source, stream.task(update))
            .await
            .map_err(native_error)
    }

    /// Group the answer's tasks under a plan with `title`.
    #[napi]
    pub async fn plan(&self, title: String) -> napi::Result<()> {
        let mut stream = self.inner.lock().await;
        self.slack
            .with_admitted_source(&self.source, stream.plan(&title))
            .await
            .map_err(native_error)
    }
}

#[napi]
pub struct NativeAgentChat {
    runtime: Arc<Runtime>,
    turns: Arc<Turns>,
    channels: Vec<Arc<Slack>>,
    running: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    closing: AtomicBool,
    close_gate: Mutex<()>,
    working: Mutex<Vec<Weak<Mutex<agentchat_core::WorkingStatus>>>>,
}

#[napi]
impl NativeAgentChat {
    #[napi(constructor)]
    pub fn new(channels_json: String, busy: Option<String>) -> napi::Result<Self> {
        let busy: Busy = busy
            .as_deref()
            .unwrap_or("steer")
            .parse()
            .map_err(native_error)?;
        let options: Vec<SlackOptions> =
            serde_json::from_str(&channels_json).map_err(native_error)?;
        if options.is_empty() {
            return Err(native_error("AgentChat requires at least one channel"));
        }
        let channels = options
            .into_iter()
            .map(|options| {
                let mut config = SlackConfig::new(options.bot_token, options.app_token);
                config.workspace_id = options.workspace_id;
                config.bot_user_id = options.bot_user_id;
                config.ack_emoji = options.ack_emoji;
                if let Some(limit) = options.max_pending_events {
                    config.max_pending_events = limit;
                }
                if let Some(timeout) = options.shutdown_timeout_ms {
                    config.shutdown_timeout = Duration::from_millis(timeout);
                }
                if let Some(url) = options.api_base_url {
                    config.api_base_url = url;
                }
                config.agent_sessions = options.agent_sessions;
                Slack::new(config).map(Arc::new).map_err(native_error)
            })
            .collect::<napi::Result<Vec<_>>>()?;
        Ok(Self {
            runtime: Arc::new(Runtime::new().with_busy(busy)),
            turns: Arc::default(),
            channels,
            running: Arc::new(AtomicBool::new(false)),
            closed: Arc::new(AtomicBool::new(false)),
            closing: AtomicBool::new(false),
            close_gate: Mutex::new(()),
            working: Mutex::new(Vec::new()),
        })
    }

    #[napi(
        ts_args_type = "handler: (channelIndex: number, messageJson: string, turnId: number) => Promise<string | null>, appHandler?: (channelIndex: number, routedJson: string) => Promise<string | null>"
    )]
    pub async fn run(
        &self,
        handler: Arc<Handler>,
        app_handler: Option<Arc<AppHandler>>,
    ) -> napi::Result<()> {
        if self.closing.load(Ordering::Acquire) || self.closed.load(Ordering::Acquire) {
            return Err(native_error("AgentChat is closed"));
        }
        if self.running.swap(true, Ordering::AcqRel) {
            return Err(native_error("AgentChat is already running"));
        }
        // The JavaScript callback must not outlive `run`, or it keeps Node alive.
        struct ClearHandlers<'a>(&'a [Arc<Slack>]);
        impl Drop for ClearHandlers<'_> {
            fn drop(&mut self) {
                for slack in self.0 {
                    slack.clear_handlers();
                }
            }
        }
        let _clear = ClearHandlers(&self.channels);
        let mut tasks = JoinSet::new();
        for (index, slack) in self.channels.iter().enumerate() {
            if let Some(app_handler) = &app_handler {
                // JavaScript matches its handlers; the route comes from the provider.
                let app_handler = Arc::clone(app_handler);
                slack.on_raw(move |payload| {
                    let app_handler = Arc::clone(&app_handler);
                    async move {
                        let routed = serde_json::json!({
                            "route": interaction::route(&payload),
                            "payload": payload,
                        });
                        let promise = app_handler
                            .call_async_catch((index as u32, routed.to_string()).into())
                            .await
                            .map_err(|e| CoreError::Handler(e.to_string()))?;
                        let body = promise
                            .await
                            .map_err(|e| CoreError::Handler(e.to_string()))?;
                        body.map(|json| serde_json::from_str(&json))
                            .transpose()
                            .map_err(|e| CoreError::Handler(e.to_string()))
                    }
                });
            }
            let slack = Arc::clone(slack);
            let runtime = Arc::clone(&self.runtime);
            let handler = Arc::clone(&handler);
            let turns = Arc::clone(&self.turns);
            tasks.spawn(async move {
                let channel: Arc<dyn Channel> = slack.clone();
                slack
                    .run(move |message| {
                        let runtime = Arc::clone(&runtime);
                        let channel = Arc::clone(&channel);
                        let handler = Arc::clone(&handler);
                        let turns = Arc::clone(&turns);
                        async move {
                            runtime
                                .receive(channel, message, move |context| {
                                    let handler = Arc::clone(&handler);
                                    let turns = Arc::clone(&turns);
                                    async move {
                                        let json = serde_json::to_string(&context.message)
                                            .map_err(|e| CoreError::Handler(e.to_string()))?;
                                        let id = turns.next.fetch_add(1, Ordering::Relaxed);
                                        if let Some(control) = context.control() {
                                            turns.controls().insert(id, control.clone());
                                        }
                                        let _entry = TurnEntry { turns: &turns, id };
                                        let promise = handler
                                            .call_async_catch((index as u32, json, id).into())
                                            .await
                                            .map_err(|e| CoreError::Handler(e.to_string()))?;
                                        promise.await.map_err(|e| CoreError::Handler(e.to_string()))
                                    }
                                })
                                .await
                        }
                    })
                    .await
            });
        }
        let mut first_error = None;
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    first_error = Some(native_error(error));
                    break;
                }
                Err(error) => {
                    first_error = Some(native_error(error));
                    break;
                }
            }
        }
        if first_error.is_some() {
            let _ = self.close().await;
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        }
        self.running.store(false, Ordering::Release);
        first_error.map_or(Ok(()), Err)
    }

    #[napi]
    pub async fn close(&self) -> napi::Result<()> {
        self.closing.store(true, Ordering::Release);
        for channel in &self.channels {
            channel.stop_admission();
        }
        let _close_guard = self.close_gate.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut first_error = None;
        let working: Vec<_> = self
            .working
            .lock()
            .await
            .drain(..)
            .filter_map(|status| status.upgrade())
            .collect();
        let mut status_closures = JoinSet::new();
        for status in working {
            status_closures.spawn(async move { status.lock().await.close().await });
        }
        while let Some(result) = status_closures.join_next().await {
            if let Err(error) = result {
                first_error.get_or_insert_with(|| native_error(error));
            }
        }
        let mut closures = JoinSet::new();
        for channel in &self.channels {
            let channel = Arc::clone(channel);
            closures.spawn(async move { channel.close().await });
        }
        while let Some(result) = closures.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    first_error.get_or_insert_with(|| native_error(error));
                }
                Err(error) => {
                    first_error.get_or_insert_with(|| native_error(error));
                }
            }
        }
        self.closed.store(true, Ordering::Release);
        first_error.map_or(Ok(()), Err)
    }

    #[napi]
    pub async fn reply(
        &self,
        channel_index: u32,
        source_json: String,
        text: String,
    ) -> napi::Result<String> {
        if self.closed.load(Ordering::Acquire) {
            return Err(native_error("AgentChat is closed"));
        }
        let source = decode_message(&source_json)?;
        let slack = self.channel(channel_index)?;
        let channel: Arc<dyn Channel> = slack.clone();
        let reply = slack
            .with_admitted_source(&source, self.runtime.reply(channel, &source, &text))
            .await
            .map_err(native_error)?;
        encode_message(&reply)
    }

    #[napi]
    pub async fn history(
        &self,
        conversation_id: String,
        limit: Option<u32>,
    ) -> napi::Result<String> {
        let history = self
            .runtime
            .history(
                &ConversationId(conversation_id),
                limit.map(|value| value as usize),
            )
            .await
            .map_err(native_error)?;
        serde_json::to_string(&history).map_err(native_error)
    }

    #[napi]
    pub async fn thread_history(
        &self,
        channel_index: u32,
        source_json: String,
        limit: u32,
    ) -> napi::Result<String> {
        let source = decode_message(&source_json)?;
        let slack = self.channel(channel_index)?;
        let channel: Arc<dyn Channel> = slack.clone();
        let context =
            agentchat_core::MessageContext::new(Arc::clone(&self.runtime), channel, source.clone());
        let history = slack
            .with_admitted_source(&source, context.history(limit as usize))
            .await
            .map_err(native_error)?;
        serde_json::to_string(&history).map_err(native_error)
    }

    #[napi]
    pub async fn subscribe(&self, channel_index: u32, source_json: String) -> napi::Result<()> {
        let source = decode_message(&source_json)?;
        let slack = self.channel(channel_index)?;
        slack
            .with_admitted_source(&source, slack.subscribe(&source))
            .await
            .map_err(native_error)
    }

    #[napi]
    pub async fn reply_rich(
        &self,
        channel_index: u32,
        source_json: String,
        content_json: String,
    ) -> napi::Result<String> {
        let source = decode_message(&source_json)?;
        let content: RichReply = serde_json::from_str(&content_json).map_err(native_error)?;
        let slack = self.channel(channel_index)?;
        let channel: Arc<dyn Channel> = slack.clone();
        let reply = slack
            .with_admitted_source(&source, self.runtime.reply_rich(channel, &source, &content))
            .await
            .map_err(native_error)?;
        encode_message(&reply)
    }

    #[napi]
    pub async fn upload_files(
        &self,
        channel_index: u32,
        source_json: String,
        files: Vec<NativeUploadFile>,
    ) -> napi::Result<String> {
        let source = decode_message(&source_json)?;
        let files: Vec<_> = files
            .into_iter()
            .map(|file| UploadFile {
                filename: file.filename,
                content: file.content.to_vec(),
                title: file.title,
            })
            .collect();
        let slack = self.channel(channel_index)?;
        let channel: Arc<dyn Channel> = slack.clone();
        let uploaded = slack
            .with_admitted_source(&source, self.runtime.upload_files(channel, &source, &files))
            .await
            .map_err(native_error)?;
        serde_json::to_string(&uploaded).map_err(native_error)
    }

    #[napi]
    pub async fn download_attachment(
        &self,
        channel_index: u32,
        source_json: String,
        attachment_json: String,
        max_bytes: u32,
    ) -> napi::Result<NativeDownloadedFile> {
        let source = decode_message(&source_json)?;
        let attachment: Attachment =
            serde_json::from_str(&attachment_json).map_err(native_error)?;
        let slack = self.channel(channel_index)?;
        let channel: Arc<dyn Channel> = slack.clone();
        let file = slack
            .with_admitted_source(
                &source,
                self.runtime
                    .download_attachment(channel, &source, &attachment, max_bytes as usize),
            )
            .await
            .map_err(native_error)?;
        Ok(NativeDownloadedFile {
            id: file.id,
            name: file.name,
            content: file.content.into(),
            media_type: file.media_type,
        })
    }

    #[napi]
    pub async fn set_status(
        &self,
        channel_index: u32,
        source_json: String,
        status: String,
    ) -> napi::Result<bool> {
        let source = decode_message(&source_json)?;
        let slack = self.channel(channel_index)?;
        let channel: Arc<dyn Channel> = slack.clone();
        slack
            .with_admitted_source(&source, self.runtime.set_status(channel, &source, &status))
            .await
            .map_err(native_error)
    }

    #[napi]
    pub async fn working(
        &self,
        channel_index: u32,
        source_json: String,
        status: String,
        refresh_interval_ms: u32,
    ) -> napi::Result<NativeWorkingStatus> {
        // Serialize creation with the shutdown snapshot so a pending status cannot
        // appear after its channel transport has been closed.
        let mut working = self.working.lock().await;
        if self.closing.load(Ordering::Acquire) {
            return Err(native_error("AgentChat is closed"));
        }
        let source = decode_message(&source_json)?;
        let slack = self.channel(channel_index)?;
        let channel: Arc<dyn Channel> = slack.clone();
        let context =
            agentchat_core::MessageContext::new(Arc::clone(&self.runtime), channel, source.clone());
        let inner = Arc::new(Mutex::new(
            slack
                .with_admitted_source(
                    &source,
                    context.working(&status, Duration::from_millis(refresh_interval_ms.into())),
                )
                .await
                .map_err(native_error)?,
        ));
        if self.closing.load(Ordering::Acquire) {
            inner.lock().await.close().await;
            return Err(native_error("AgentChat is closed"));
        }
        working.retain(|status| status.strong_count() > 0);
        working.push(Arc::downgrade(&inner));
        Ok(NativeWorkingStatus { inner })
    }

    /// Messages steered to a running handler since it last asked, as JSON.
    #[napi]
    pub fn steered(&self, turn_id: u32) -> napi::Result<String> {
        let messages = self.turn(turn_id).map(|t| t.steered()).unwrap_or_default();
        serde_json::to_string(&messages).map_err(native_error)
    }

    #[napi]
    pub fn stopped(&self, turn_id: u32) -> bool {
        self.turn(turn_id).is_some_and(|turn| turn.stopped())
    }

    /// Resolves true once the turn is asked to stop, or false if it ended first.
    #[napi]
    pub async fn wait_stopped(&self, turn_id: u32) -> bool {
        match self.turn(turn_id) {
            Some(turn) => {
                let stopped = turn.until_stopped();
                drop(turn);
                stopped.await
            }
            None => false,
        }
    }

    /// Ask a conversation's running handler to stop; false when none is running.
    #[napi]
    pub fn stop(&self, conversation_id: String) -> bool {
        self.runtime.stop(&ConversationId(conversation_id))
    }

    /// Returns `{"answer": ...}`, or `{"error": "timeout" | "unsupported"}`.
    #[napi]
    #[allow(clippy::too_many_arguments)]
    pub async fn ask(
        &self,
        channel_index: u32,
        source_json: String,
        text: String,
        choices_json: String,
        timeout_ms: u32,
        allowed_json: Option<String>,
    ) -> napi::Result<String> {
        let source = decode_message(&source_json)?;
        let choices: Vec<Choice> = serde_json::from_str(&choices_json).map_err(native_error)?;
        let options = AskOptions {
            timeout: Duration::from_millis(timeout_ms.into()),
            allowed: allowed(allowed_json)?,
        };
        let slack = self.channel(channel_index)?;
        let channel: Arc<dyn Channel> = slack.clone();
        let result = slack
            .with_admitted_source(
                &source,
                self.runtime.ask(channel, &source, &text, choices, options),
            )
            .await;
        let value = match result {
            Ok(answer) => serde_json::json!({"answer": answer}),
            Err(CoreError::Timeout(_)) => serde_json::json!({"error": "timeout"}),
            Err(CoreError::Unsupported(_)) => serde_json::json!({"error": "unsupported"}),
            Err(error) => return Err(native_error(error)),
        };
        Ok(value.to_string())
    }

    /// Show a Stop button for the running handler; the shown message as JSON, or null.
    #[napi]
    pub async fn offer_stop(
        &self,
        channel_index: u32,
        source_json: String,
        turn_id: u32,
        text: String,
        label: String,
        allowed_json: Option<String>,
    ) -> napi::Result<Option<String>> {
        let source = decode_message(&source_json)?;
        let options = StopOptions {
            text,
            label,
            allowed: allowed(allowed_json)?,
        };
        let turn = self
            .turn(turn_id)
            .ok_or_else(|| native_error("This handler is no longer running"))?;
        let slack = self.channel(channel_index)?;
        let channel: Arc<dyn Channel> = slack.clone();
        let shown = slack
            .with_admitted_source(
                &source,
                self.runtime.offer_stop_for(channel, &source, options, turn),
            )
            .await
            .map_err(native_error)?;
        shown.map(|message| encode_message(&message)).transpose()
    }

    #[napi]
    pub fn stream(
        &self,
        channel_index: u32,
        source_json: String,
        interval_ms: Option<u32>,
    ) -> napi::Result<NativeAnswerStream> {
        let source = decode_message(&source_json)?;
        let slack = self.channel(channel_index)?;
        let channel: Arc<dyn Channel> = slack.clone();
        let interval = interval_ms.map(|ms| Duration::from_millis(ms.into()));
        let stream = self.runtime.stream(channel, &source, interval);
        Ok(NativeAnswerStream {
            slack,
            source,
            inner: Arc::new(Mutex::new(stream)),
        })
    }

    #[napi]
    pub async fn mirror(
        &self,
        channel_index: u32,
        source_json: String,
        message_json: String,
        origin: String,
    ) -> napi::Result<String> {
        let source = decode_message(&source_json)?;
        let message = decode_message(&message_json)?;
        let slack = self.channel(channel_index)?;
        let channel: Arc<dyn Channel> = slack.clone();
        let mirrored = slack
            .with_admitted_source(
                &source,
                self.runtime.mirror(channel, &source, &message, &origin),
            )
            .await
            .map_err(native_error)?;
        encode_message(&mirrored)
    }
}

#[napi]
impl NativeAgentChat {
    /// Post a notice only `userId` can see; the sent message as JSON.
    #[napi]
    pub async fn send_ephemeral(
        &self,
        channel_index: u32,
        source_json: String,
        user_id: String,
        content_json: String,
    ) -> napi::Result<String> {
        let source = decode_message(&source_json)?;
        let content: RichReply = serde_json::from_str(&content_json).map_err(native_error)?;
        let slack = self.channel(channel_index)?;
        let channel: Arc<dyn Channel> = slack.clone();
        let sent = slack
            .with_admitted_source(
                &source,
                self.runtime
                    .send_ephemeral(channel, &source, &user_id, &content),
            )
            .await
            .map_err(native_error)?;
        encode_message(&sent)
    }

    /// Delete a message this app sent.
    #[napi]
    pub async fn delete_message(
        &self,
        channel_index: u32,
        message_json: String,
    ) -> napi::Result<()> {
        let message = decode_message(&message_json)?;
        let slack = self.channel(channel_index)?;
        let channel: Arc<dyn Channel> = slack.clone();
        self.runtime
            .delete(channel, &message)
            .await
            .map_err(native_error)
    }

    /// `views.open` (`push` false) or `views.push`; the view as JSON.
    #[napi]
    pub async fn open_view(
        &self,
        channel_index: u32,
        trigger_id: String,
        view_json: String,
        push: bool,
    ) -> napi::Result<String> {
        let view: serde_json::Value = serde_json::from_str(&view_json).map_err(native_error)?;
        let slack = self.channel(channel_index)?;
        let opened = if push {
            slack.push_view(&trigger_id, &view).await
        } else {
            slack.open_view(&trigger_id, &view).await
        }
        .map_err(native_error)?;
        Ok(opened.to_string())
    }

    /// `views.update` by `viewId` or `externalId`; the view as JSON.
    #[napi]
    pub async fn update_view(
        &self,
        channel_index: u32,
        view_json: String,
        view_id: Option<String>,
        external_id: Option<String>,
        hash: Option<String>,
    ) -> napi::Result<String> {
        let view: serde_json::Value = serde_json::from_str(&view_json).map_err(native_error)?;
        let target = match (view_id, external_id) {
            (Some(id), None) => ViewTarget::Id(id),
            (None, Some(id)) => ViewTarget::External(id),
            _ => return Err(native_error("Pass exactly one of viewId or externalId")),
        };
        let slack = self.channel(channel_index)?;
        let updated = slack
            .update_view(&view, &target, hash.as_deref())
            .await
            .map_err(native_error)?;
        Ok(updated.to_string())
    }

    /// `views.publish` of a user's App Home; the view as JSON.
    #[napi]
    pub async fn publish_home(
        &self,
        channel_index: u32,
        user_id: String,
        view_json: String,
        hash: Option<String>,
    ) -> napi::Result<String> {
        let view: serde_json::Value = serde_json::from_str(&view_json).map_err(native_error)?;
        let slack = self.channel(channel_index)?;
        let published = slack
            .publish_home(&user_id, &view, hash.as_deref())
            .await
            .map_err(native_error)?;
        Ok(published.to_string())
    }
}

impl NativeAgentChat {
    fn turn(&self, turn_id: u32) -> Option<TurnControl> {
        self.turns.get(turn_id)
    }

    fn channel(&self, index: u32) -> napi::Result<Arc<Slack>> {
        if self.closed.load(Ordering::Acquire) {
            return Err(native_error("AgentChat is closed"));
        }
        self.channels
            .get(index as usize)
            .cloned()
            .ok_or_else(|| native_error("Unknown channel"))
    }
}
