//! Slack policies shared by native transports and foreign SDK adapters.
use super::{events, route::SlackTimestamp, SlackUser};
use agentchat_core::{Error, HistoryLimit, Message, Result, Role, Sender, StreamUpdate};
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};

#[derive(Clone, Debug)]
pub struct ReplyDestination {
    pub channel_id: String,
    pub thread_ts: Option<String>,
}

impl ReplyDestination {
    pub fn from_source(source: &Message, workspace: Option<&str>) -> Result<Self> {
        if source.channel != "slack"
            || workspace.is_some_and(|id| {
                source.metadata.get("team_id").and_then(Value::as_str) != Some(id)
            })
        {
            return Err(Error::InvalidInput(
                "Expected a source in the configured Slack workspace".into(),
            ));
        }
        let channel_id = source
            .metadata
            .get("channel_id")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| Error::InvalidInput("Expected a Slack channel destination".into()))?;
        let thread_ts = match source.metadata.get("reply_thread_timestamp") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
            _ => {
                return Err(Error::InvalidInput(
                    "Expected a Slack thread destination".into(),
                ))
            }
        };
        Ok(Self {
            channel_id: channel_id.into(),
            thread_ts,
        })
    }

    pub fn validate(channel_id: &str, thread_ts: Option<&str>) -> Result<Self> {
        validate_channel(channel_id)?;
        if thread_ts.is_some_and(|value| SlackTimestamp::parse(value).is_none()) {
            return Err(Error::InvalidInput(
                "Expected a Slack thread destination".into(),
            ));
        }
        Ok(Self {
            channel_id: channel_id.into(),
            thread_ts: thread_ts.map(str::to_owned),
        })
    }
}

pub fn user_profile(response: &Value, user_id: &str) -> Result<SlackUser> {
    let user = &response["user"];
    if response["ok"] != true || !user.is_object() {
        return Err(Error::InvalidInput(
            "Slack did not return a user profile".into(),
        ));
    }
    let invalid = || Error::InvalidInput("Slack returned an invalid user profile".into());
    let profile = user["profile"].as_object().ok_or_else(invalid)?;
    if user["id"] != user_id {
        return Err(invalid());
    }
    let text = |value: &Value| {
        value
            .as_str()
            .map(events::trim_python)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    Ok(SlackUser {
        id: user_id.into(),
        team_id: events::nonempty(&user["team_id"])
            .ok_or_else(invalid)?
            .into(),
        display_name: profile
            .get("display_name")
            .and_then(text)
            .or_else(|| profile.get("real_name").and_then(text))
            .or_else(|| text(&user["name"])),
        email: profile.get("email").and_then(text),
        is_bot: user["is_bot"].as_bool().ok_or_else(invalid)? || user["is_app_user"] == true,
        deleted: user["deleted"].as_bool().ok_or_else(invalid)?,
    })
}

pub struct ThreadHistory {
    source: Message,
    bot_user_id: Option<String>,
    thread: String,
    current: SlackTimestamp,
    root: Option<Message>,
    recent: VecDeque<Message>,
    limit: HistoryLimit,
    cursor: String,
    seen: HashSet<String>,
    pages: usize,
}
impl ThreadHistory {
    pub fn new(
        source: Message,
        bot_user_id: Option<String>,
        workspace: Option<&str>,
        limit: usize,
    ) -> Result<Self> {
        let limit = HistoryLimit::new(limit)?;
        let thread = source
            .metadata
            .get("reply_thread_timestamp")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::InvalidInput("Expected a thread in the configured Slack workspace".into())
            })?
            .to_owned();
        if workspace
            .is_some_and(|id| source.metadata.get("team_id").and_then(Value::as_str) != Some(id))
        {
            return Err(Error::InvalidInput(
                "Expected a thread in the configured Slack workspace".into(),
            ));
        }
        let current = source
            .metadata
            .get("message_timestamp")
            .and_then(Value::as_str)
            .and_then(SlackTimestamp::parse)
            .ok_or_else(|| Error::InvalidInput("Expected a Slack message timestamp".into()))?;
        Ok(Self {
            source,
            bot_user_id,
            thread,
            current,
            root: None,
            recent: VecDeque::new(),
            limit,
            cursor: String::new(),
            seen: HashSet::new(),
            pages: 0,
        })
    }
    pub fn is_root(&self) -> bool {
        self.current.as_str() == self.thread
    }
    pub fn request(&self) -> Value {
        json!({"channel":self.source.metadata.get("channel_id"),"ts":self.thread,"latest":self.current.as_str(),"inclusive":false,"limit":100,"cursor":self.cursor,"include_all_metadata":true})
    }
    pub fn accept(&mut self, response: &Value) -> Result<bool> {
        if response["ok"] != true {
            return Err(Error::InvalidInput(
                "Slack did not return thread history".into(),
            ));
        }
        let records = response["messages"]
            .as_array()
            .ok_or_else(|| Error::InvalidInput("Slack did not return thread history".into()))?;
        for record in records {
            let Some(input) = events::history_record(record, self.bot_user_id.as_deref()) else {
                continue;
            };
            let ts = &input.timestamp;
            if !ts.is_before(&self.current) {
                continue;
            }
            let item = Message {
                id: format!(
                    "slack:{}:{}:{ts}",
                    self.source.metadata["team_id"].as_str().unwrap_or_default(),
                    self.source.metadata["channel_id"]
                        .as_str()
                        .unwrap_or_default(),
                )
                .into(),
                conversation_id: self.source.conversation_id.clone(),
                channel: self.source.channel.clone(),
                sender: input.sender,
                text: input.text,
                role: input.role,
                thread_id: Some(self.thread.clone()),
                addressed: true,
                attachments: input.files,
                metadata: json!({
                    "message_timestamp": ts.as_str(),
                    "team_id": self.source.metadata.get("team_id"),
                    "channel_id": self.source.metadata.get("channel_id"),
                    "reply_thread_timestamp": self.thread,
                    "slack_reference": input.reference,
                })
                .as_object()
                .cloned()
                .unwrap_or_default(),
            };
            if ts.as_str() == self.thread {
                self.root = Some(item);
            } else {
                self.recent.push_back(item);
                if self.recent.len() > self.limit.get() - 1 {
                    self.recent.pop_front();
                }
            }
        }
        self.pages += 1;
        if response
            .get("has_more")
            .is_none_or(|value| value.is_null() || value == false)
        {
            return Ok(true);
        }
        let next =
            events::nonempty(&response["response_metadata"]["next_cursor"]).ok_or_else(|| {
                Error::InvalidInput("Slack returned invalid history pagination".into())
            })?;
        if !self.seen.insert(next.into()) {
            return Err(Error::InvalidInput(
                "Slack returned invalid history pagination".into(),
            ));
        }
        if self.pages >= 5 {
            return Err(Error::InvalidInput(
                "Slack thread is too long; start a new thread".into(),
            ));
        }
        self.cursor = next.into();
        Ok(false)
    }
    pub fn messages(self) -> Vec<Message> {
        self.root.into_iter().chain(self.recent).collect()
    }
}

/// Validate a Slack channel ID once at the provider boundary.
pub fn validate_channel(channel_id: &str) -> Result<()> {
    if channel_id.len() < 2
        || !matches!(channel_id.as_bytes()[0], b'C' | b'D' | b'G')
        || !channel_id
            .bytes()
            .skip(1)
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return Err(Error::InvalidInput(
            "Expected a Slack channel destination".into(),
        ));
    }
    Ok(())
}

/// The operation whose successful write requires a provider receipt.
#[derive(Clone, Copy, Debug)]
pub enum DeliveryKind {
    Reply,
    RichReply,
    Mirror,
    Stream,
    Ephemeral,
}
impl DeliveryKind {
    fn label(self) -> &'static str {
        match self {
            Self::Reply => "reply",
            Self::RichReply => "rich reply",
            Self::Mirror => "mirror",
            Self::Stream => "streamed reply",
            Self::Ephemeral => "ephemeral message",
        }
    }
}

/// A provider receipt; its contents stay lossless, including timestamp precision.
#[derive(Clone, Debug)]
pub struct MessageReceipt {
    timestamp: String,
}
impl MessageReceipt {
    pub fn confirmed(response: &Value, kind: DeliveryKind) -> Result<Self> {
        let invalid = || {
            Error::Transport(format!(
                "Slack {} delivery could not be confirmed",
                kind.label()
            ))
        };
        if response["ok"] != true {
            return Err(invalid());
        }
        let timestamp = response["ts"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(invalid)?;
        Ok(Self {
            timestamp: timestamp.into(),
        })
    }
    pub fn confirmed_timestamp(response: &Value, kind: DeliveryKind) -> Result<Self> {
        let receipt = Self::confirmed(response, kind)?;
        if SlackTimestamp::parse(&receipt.timestamp).is_none() {
            return Err(Error::Transport(format!(
                "Slack {} delivery could not be confirmed",
                kind.label()
            )));
        }
        Ok(receipt)
    }
    /// The original Python text API accepted SDK response timestamps without
    /// receipt validation. Keep that compatibility decision explicit and local.
    pub fn compatibility_text(response: &Value) -> Self {
        let timestamp = response
            .get("ts")
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string())
            })
            .unwrap_or_else(|| "unknown".into());
        Self { timestamp }
    }
}

/// Outbound routing metadata, already interpreted by the owning transport.
#[derive(Clone, Debug)]
pub struct ReplyMessageRoute {
    pub channel_id: String,
    pub thread_timestamp: Value,
    pub team_id: Option<String>,
}
impl ReplyMessageRoute {
    /// Retain the legacy SDK adapter's permissive text routing and wire metadata.
    pub fn compatibility_source(source: &Message) -> Self {
        Self {
            channel_id: source
                .metadata
                .get("channel_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            thread_timestamp: source
                .metadata
                .get("reply_thread_timestamp")
                .cloned()
                .unwrap_or(Value::Null),
            team_id: None,
        }
    }
    pub fn validated(destination: &ReplyDestination) -> Self {
        Self {
            channel_id: destination.channel_id.clone(),
            thread_timestamp: json!(destination.thread_ts),
            team_id: None,
        }
    }
}

/// Mirror attribution carries the original identity together with its origin.
pub struct MirroredReply<'a> {
    pub message: &'a Message,
    pub origin: &'a str,
}
impl MirroredReply<'_> {
    pub fn apply(&self, reply: &mut Message) {
        reply.sender = self.message.sender.clone();
        reply.role = Role::User;
        reply
            .metadata
            .insert("mirrored_message_id".into(), json!(self.message.id));
        reply.metadata.insert("origin".into(), self.origin.into());
    }
}

/// Most characters one Slack stream may hold in total; longer answers continue
/// in a new stream. Counted in UTF-16 units, which is never fewer.
pub const STREAM_LIMIT: usize = 12_000;

/// The unsent end of a streamed message; Slack streams can only grow.
pub fn stream_delta<'a>(sent: &str, text: &'a str) -> Result<&'a str> {
    text.strip_prefix(sent).ok_or_else(|| {
        Error::InvalidInput("A Slack stream can only add text to what it already sent".into())
    })
}

/// `chat.startStream` in a thread, addressed to the person who asked.
pub fn stream_start_request(channel: &str, thread_ts: &str, text: &str, source: &Message) -> Value {
    let mut request = json!({
        "channel": channel,
        "thread_ts": thread_ts,
        "markdown_text": text,
        "recipient_user_id": source.sender.id,
    });
    if let Some(team) = source
        .metadata
        .get("team_id")
        .filter(|team| team.is_string())
    {
        request["recipient_team_id"] = team.clone();
    }
    request
}

/// `chat.appendStream` (`finishing` false) or `chat.stopStream` for the text that
/// `handle` has not sent yet; `None` when an append has nothing to add. Returns the
/// request and the handle to use next. A stream started with task updates sends
/// its text as `markdown_text` chunks: Slack rejects mixing the two forms.
pub fn stream_request(
    handle: &Value,
    text: &str,
    finishing: bool,
) -> Result<(Option<Value>, Value)> {
    let sent = handle["sent"].as_str().unwrap_or_default();
    let delta = stream_delta(sent, text)?;
    let mut request = json!({"channel": handle["channel"], "ts": handle["ts"]});
    if !delta.is_empty() {
        if handle["mode"] == "chunks" {
            request["chunks"] = json!([{"type": "markdown_text", "text": delta}]);
        } else {
            request["markdown_text"] = json!(delta);
        }
    } else if !finishing {
        return Ok((None, handle.clone()));
    }
    let mut next = handle.clone();
    next["sent"] = json!(text);
    Ok((Some(request), next))
}

/// Task titles and details are shown as text; escape Slack's control characters
/// so model output cannot create mentions or links.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Slack `plan_update` and `task_update` chunks for stream updates.
pub fn task_chunks(updates: &[StreamUpdate]) -> Vec<Value> {
    updates
        .iter()
        .map(|update| match update {
            StreamUpdate::Plan { title } => json!({"type": "plan_update", "title": escape(title)}),
            StreamUpdate::Task(task) => {
                let mut chunk = json!({
                    "type": "task_update",
                    "id": task.id,
                    "title": escape(&task.title),
                    "status": task.status,
                });
                if let Some(details) = &task.details {
                    chunk["details"] = json!(escape(details));
                }
                if let Some(output) = &task.output {
                    chunk["output"] = json!(escape(output));
                }
                chunk
            }
        })
        .collect()
}

/// `chat.startStream` in chunk mode, opening with task and plan updates. Tasks
/// show together in a plan block when a plan is named, otherwise as a timeline.
pub fn stream_start_tasks_request(
    channel: &str,
    thread_ts: &str,
    source: &Message,
    updates: &[StreamUpdate],
) -> Value {
    let mut request = stream_start_request(channel, thread_ts, "", source);
    if let Some(object) = request.as_object_mut() {
        object.remove("markdown_text");
    }
    let plan = updates
        .iter()
        .any(|update| matches!(update, StreamUpdate::Plan { .. }));
    request["task_display_mode"] = json!(if plan { "plan" } else { "timeline" });
    request["chunks"] = json!(task_chunks(updates));
    request
}

/// The handle of a stream that started in chunk mode.
pub fn chunk_stream_handle(channel: &str, ts: &Value) -> Value {
    json!({"channel": channel, "ts": ts, "sent": "", "mode": "chunks", "tasks": true})
}

/// `chat.appendStream` with task updates, or `None` when this stream can no longer
/// show them (it streams plain text, or Slack refused an earlier update).
pub fn stream_tasks_request(handle: &Value, updates: &[StreamUpdate]) -> Option<Value> {
    if handle["mode"] != "chunks" || handle["tasks"] == false || updates.is_empty() {
        return None;
    }
    Some(json!({"channel": handle["channel"], "ts": handle["ts"], "chunks": task_chunks(updates)}))
}

/// The same stream, without task updates from now on.
pub fn without_tasks(handle: &Value) -> Value {
    let mut next = handle.clone();
    next["tasks"] = json!(false);
    next
}

/// Whether a refused chunk-mode start may be retried as a plain text stream: only
/// for a definite rejection that is not a rate limit.
pub fn retry_without_chunks(code: &str) -> bool {
    code != "ratelimited"
}

/// The working status request. Slack's agent sessions take a lifecycle status
/// (`processing` while working, `active` when ready) and show their own loading
/// text; the assistant thread status shows `status` itself.
pub fn status_request(
    channel_id: &str,
    thread_ts: &str,
    status: &str,
    agent_sessions: bool,
) -> (&'static str, Value) {
    if agent_sessions {
        let lifecycle = if status.is_empty() {
            "active"
        } else {
            "processing"
        };
        (
            "agents.sessions.setStatus",
            json!({"channel_id": channel_id, "thread_ts": thread_ts, "status": lifecycle}),
        )
    } else {
        (
            "assistant.threads.setStatus",
            json!({"channel_id": channel_id, "thread_ts": thread_ts, "status": status}),
        )
    }
}

pub fn reply_message(
    source: &Message,
    route: ReplyMessageRoute,
    text: String,
    receipt: MessageReceipt,
    mirror: Option<MirroredReply<'_>>,
) -> Message {
    let mut metadata = json!({"channel_id":route.channel_id,"reply_thread_timestamp":route.thread_timestamp,"message_timestamp":receipt.timestamp});
    if let Some(team_id) = route.team_id {
        metadata["team_id"] = team_id.into();
    }
    let mut reply = Message {
        id: format!("slack:{}:{}", route.channel_id, receipt.timestamp).into(),
        conversation_id: source.conversation_id.clone(),
        channel: "slack".into(),
        sender: Sender {
            id: "agentchat".into(),
            display_name: Some("AgentChat".into()),
        },
        text,
        role: Role::Assistant,
        metadata: metadata.as_object().cloned().unwrap_or_default(),
        thread_id: source.thread_id.clone(),
        addressed: true,
        attachments: vec![],
    };
    if let Some(mirror) = mirror {
        mirror.apply(&mut reply);
    }
    reply
}

#[cfg(test)]
mod delivery_tests {
    use super::*;
    fn source() -> Message {
        Message {
            id: "incoming".into(),
            conversation_id: "thread".into(),
            channel: "slack".into(),
            sender: Sender {
                id: "user".into(),
                display_name: Some("雪".into()),
            },
            text: "input".into(),
            role: Role::User,
            metadata: json!({"channel_id":"C123","reply_thread_timestamp":"1700000000.000001"})
                .as_object()
                .unwrap()
                .clone(),
            thread_id: Some("1700000000.000001".into()),
            addressed: true,
            attachments: vec![],
        }
    }
    #[test]
    fn confirmed_receipts_preserve_precision_and_reject_uncertain_writes() {
        let response = json!({"ok":true,"ts":"9007199254740993.00000001"});
        let receipt = MessageReceipt::confirmed_timestamp(&response, DeliveryKind::Reply).unwrap();
        let source = source();
        let reply = reply_message(
            &source,
            ReplyMessageRoute::compatibility_source(&source),
            "answer".into(),
            receipt,
            None,
        );
        assert_eq!(reply.id.as_ref(), "slack:C123:9007199254740993.00000001");
        assert_eq!(reply.conversation_id, source.conversation_id);
        assert_eq!(reply.thread_id, source.thread_id);
        for response in [
            json!({"ok":false,"ts":"1.1"}),
            json!({"ok":true}),
            json!({"ok":true,"ts":5}),
        ] {
            assert!(MessageReceipt::confirmed(&response, DeliveryKind::RichReply).is_err());
        }
        assert!(MessageReceipt::confirmed_timestamp(
            &json!({"ok":true,"ts":"bad"}),
            DeliveryKind::Reply
        )
        .is_err());
    }
    #[test]
    fn explicit_legacy_policy_and_mirror_attribution_are_preserved() {
        let source = source();
        let reply = reply_message(
            &source,
            ReplyMessageRoute::compatibility_source(&source),
            source.text.clone(),
            MessageReceipt::compatibility_text(&json!({})),
            Some(MirroredReply {
                message: &source,
                origin: "web",
            }),
        );
        assert_eq!(reply.id.as_ref(), "slack:C123:unknown");
        assert_eq!(reply.sender, source.sender);
        assert_eq!(reply.role, Role::User);
        assert_eq!(reply.metadata["mirrored_message_id"], "incoming");
        assert_eq!(reply.metadata["origin"], "web");
    }
}

/// Bounded, insertion-refreshed subscription memory shared by every transport.
#[derive(Default)]
pub struct SubscriptionCache {
    conversations: VecDeque<String>,
}
impl SubscriptionCache {
    pub fn contains(&self, conversation_id: &str) -> bool {
        self.conversations.iter().any(|id| id == conversation_id)
    }
    pub fn add(&mut self, conversation_id: &str) {
        self.conversations.retain(|id| id != conversation_id);
        self.conversations.push_back(conversation_id.to_owned());
        if self.conversations.len() > 1000 {
            self.conversations.pop_front();
        }
    }
}
#[cfg(test)]
mod stream_tests {
    use super::*;
    use agentchat_core::{TaskStatus, TaskUpdate};

    #[test]
    fn chunk_streams_send_text_as_chunks_and_escape_task_text() {
        let handle = chunk_stream_handle("C1", &json!("1.5"));
        let (request, next) = stream_request(&handle, "Hello", false).unwrap();
        let request = request.unwrap();
        assert_eq!(
            request["chunks"],
            json!([{"type": "markdown_text", "text": "Hello"}])
        );
        assert!(request.get("markdown_text").is_none());
        assert_eq!(next["mode"], "chunks");
        let mut task = TaskUpdate::new("t1", "Read <@U1> & co", TaskStatus::InProgress);
        task.details = Some("<http://x|y>".into());
        let updates = [
            StreamUpdate::Plan {
                title: "Plan".into(),
            },
            StreamUpdate::Task(task),
        ];
        let chunks = task_chunks(&updates);
        assert_eq!(chunks[0], json!({"type": "plan_update", "title": "Plan"}));
        assert_eq!(chunks[1]["title"], "Read &lt;@U1&gt; &amp; co");
        assert_eq!(chunks[1]["status"], "in_progress");
        assert_eq!(chunks[1]["details"], "&lt;http://x|y&gt;");
        let source = Message {
            id: "m".into(),
            conversation_id: "c".into(),
            channel: "slack".into(),
            sender: Sender {
                id: "U1".into(),
                display_name: None,
            },
            text: "x".into(),
            role: Role::User,
            metadata: json!({"team_id": "T1"}).as_object().unwrap().clone(),
            thread_id: None,
            addressed: true,
            attachments: vec![],
        };
        let start = stream_start_tasks_request("C1", "1.0", &source, &updates);
        assert_eq!(start["task_display_mode"], "plan");
        assert!(start.get("markdown_text").is_none());
        assert_eq!(start["recipient_team_id"], "T1");
        assert!(stream_tasks_request(&handle, &updates).is_some());
        assert!(stream_tasks_request(&without_tasks(&handle), &updates).is_none());
        let text = json!({"channel": "C1", "ts": "1.5", "sent": ""});
        assert!(stream_tasks_request(&text, &updates).is_none());
        assert!(!retry_without_chunks("ratelimited") && retry_without_chunks("invalid_arguments"));
    }

    #[test]
    fn agent_sessions_take_a_lifecycle_status() {
        let (method, body) = status_request("C1", "1.0", "is reading…", true);
        assert_eq!(method, "agents.sessions.setStatus");
        assert_eq!(body["status"], "processing");
        assert_eq!(status_request("C1", "1.0", "", true).1["status"], "active");
        let (method, body) = status_request("C1", "1.0", "is reading…", false);
        assert_eq!(method, "assistant.threads.setStatus");
        assert_eq!(body["status"], "is reading…");
    }
}

#[cfg(test)]
mod subscription_tests {
    use super::SubscriptionCache;
    #[test]
    fn adding_refreshes_subscription_before_bounded_eviction() {
        let mut cache = SubscriptionCache::default();
        for id in 0..1000 {
            cache.add(&id.to_string());
        }
        cache.add("0");
        cache.add("1000");
        assert!(cache.contains("0"));
        assert!(!cache.contains("1"));
        assert!(cache.contains("1000"));
    }
}
