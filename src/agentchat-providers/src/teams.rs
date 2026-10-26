//! Authenticated Teams activity interpretation and continuation routing.
use crate::wire::{quote, strip, truthy};
use agentchat_core::{Attachment, Message, Role, Sender};
use serde_json::{json, Value};
pub fn text_payload(content: &str) -> Result<&str, &'static str> {
    if strip(content).is_empty() {
        return Err("Teams replies require nonempty text");
    }
    if content.len() > 24000 {
        return Err("Teams text replies must fit 24,000 UTF-8 bytes; send separate replies");
    }
    Ok(content)
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConversationKind {
    Personal,
    GroupChat,
    Channel,
}
impl ConversationKind {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "personal" => Some(Self::Personal),
            "groupChat" => Some(Self::GroupChat),
            "channel" => Some(Self::Channel),
            _ => None,
        }
    }
}
struct ConversationRoute<'a> {
    base_id: &'a str,
    root_id: Option<&'a str>,
}
impl<'a> ConversationRoute<'a> {
    fn from_activity(
        kind: ConversationKind,
        conversation_id: &'a str,
        activity_id: &'a str,
        reply_to: Option<&'a Value>,
    ) -> Option<Self> {
        if kind != ConversationKind::Channel {
            return Some(Self {
                base_id: conversation_id,
                root_id: None,
            });
        }
        let (base_id, suffix) = conversation_id
            .split_once(";messageid=")
            .unwrap_or((conversation_id, ""));
        let root_id = match reply_to {
            Some(value) if truthy(value) => value.as_str()?,
            _ if !suffix.is_empty() => suffix,
            _ => activity_id,
        };
        Some(Self {
            base_id,
            root_id: Some(root_id),
        })
    }
}
pub fn normalize(event: &Value, app_id: &str, tenant_id: &str) -> Option<Message> {
    if event.get("type")?.as_str() != Some("message")
        || event.get("channelId")?.as_str() != Some("msteams")
    {
        return None;
    }
    let conversation = event.get("conversation")?.as_object()?;
    let sender = event.get("from")?.as_object()?;
    let recipient = event.get("recipient")?.as_object()?;
    if event.get("channelData")?.get("tenant")?.get("id")?.as_str() != Some(tenant_id) {
        return None;
    }
    let recipient_id = recipient.get("id")?.as_str()?;
    if recipient_id != app_id && recipient_id != format!("28:{app_id}") {
        return None;
    }
    let activity_id = event.get("id")?.as_str()?;
    let sender_id = sender.get("id")?.as_str()?;
    let conversation_id = conversation.get("id")?.as_str()?;
    let service = event.get("serviceUrl")?.as_str()?;
    if [activity_id, sender_id, conversation_id, service]
        .iter()
        .any(|v| v.is_empty())
        || sender_id == recipient_id
        || sender.get("role").and_then(Value::as_str) == Some("bot")
    {
        return None;
    }
    let kind = ConversationKind::parse(conversation.get("conversationType")?.as_str()?)?;
    // A press on a prompt card: Action.Submit returns the button's data as `value`.
    let answer = event
        .get("value")
        .and_then(|v| v.get("agentchat"))
        .and_then(Value::as_str)
        .and_then(agentchat_core::answer_metadata);
    let mut text = match event.get("text") {
        Some(v) if truthy(v) => v.as_str()?.to_owned(),
        _ => String::new(),
    };
    let mut mentioned = false;
    for entity in event
        .get("entities")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if entity.get("type").and_then(Value::as_str) == Some("mention")
            && entity
                .get("mentioned")
                .and_then(|v| v.get("id"))
                .and_then(Value::as_str)
                == Some(recipient_id)
        {
            mentioned = true;
            if let Some(token) = entity.get("text").and_then(Value::as_str) {
                text = text.replace(token, "");
            }
        }
    }
    if answer.is_none() && kind != ConversationKind::Personal && !mentioned {
        return None;
    }
    let attachments = event
        .get("attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, a)| {
            a.as_object()?;
            Some(Attachment {
                id: a
                    .get("content")
                    .and_then(|v| v.get("uniqueId"))
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{activity_id}:{index}")),
                name: a.get("name").and_then(Value::as_str).map(str::to_owned),
                media_type: a
                    .get("contentType")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                size: None,
            })
        })
        .collect::<Vec<_>>();
    let text = strip(&text).to_owned();
    if answer.is_none() && text.is_empty() && attachments.is_empty() {
        return None;
    }
    // A press replies to the card, not the thread root; the root is in the ID.
    let reply_to = if answer.is_some() && conversation_id.contains(";messageid=") {
        None
    } else {
        event.get("replyToId")
    };
    let routing = ConversationRoute::from_activity(kind, conversation_id, activity_id, reply_to)?;
    let root = routing.root_id.map(str::to_owned);
    let base = routing.base_id;
    let mut identity = format!("{}:{}:{}", quote(app_id), quote(tenant_id), quote(base));
    if let Some(root) = &root {
        identity.push(':');
        identity.push_str(&quote(root));
    }
    let mut route = json!({"type":"message","channelId":"msteams","serviceUrl":service,"conversation":conversation,"from":sender,"recipient":recipient,"channelData":{"tenant":{"id":tenant_id}}});
    if let Some(root) = &root {
        route["id"] = root.clone().into();
    }
    Some(Message {
        id: format!(
            "teams:{app_id}:{tenant_id}:{}:{activity_id}",
            quote(conversation_id)
        )
        .into(),
        conversation_id: format!("teams:{identity}").into(),
        channel: "teams".into(),
        sender: Sender {
            id: sender_id.into(),
            display_name: sender
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        text,
        role: Role::User,
        metadata: {
            let mut metadata = json!({
                "app_id": app_id,
                "tenant_id": tenant_id,
                "activity_id": activity_id,
                "continuation_activity": route,
            });
            if let Some(answer) = answer {
                metadata[agentchat_core::ANSWER_METADATA] = answer;
            }
            metadata.as_object()?.clone()
        },
        thread_id: root,
        addressed: true,
        attachments,
    })
}
/// A prompt as an Adaptive Card with one Submit button per choice.
pub fn prompt_card(
    text: &str,
    prompt: &str,
    choices: &[(String, String, String)],
) -> Result<Value, &'static str> {
    text_payload(text)?;
    let actions = choices
        .iter()
        .map(|(id, label, style)| {
            let style = match style.as_str() {
                "primary" => "positive",
                "danger" => "destructive",
                _ => "default",
            };
            agentchat_core::prompt_token(prompt, id)
                .map(|token| {
                    json!({"type": "Action.Submit", "title": label, "style": style,
                           "data": {"agentchat": token}})
                })
                .map_err(|_| "Invalid prompt or choice ID")
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({
        "contentType": "application/vnd.microsoft.card.adaptive",
        "content": {
            "type": "AdaptiveCard",
            "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
            "version": "1.4",
            "body": [{"type": "TextBlock", "text": text, "wrap": true}],
            "actions": actions,
        },
    }))
}
pub fn validate_service_url(raw: &str, allowed_host: bool) -> Result<(), &'static str> {
    let invalid = "Teams service URL is not an allowed HTTPS Microsoft endpoint";
    let url = url::Url::parse(raw).map_err(|_| invalid)?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port().is_some_and(|p| p != 443)
        || !allowed_host
    {
        return Err(invalid);
    }
    Ok(())
}
#[derive(Debug, Clone)]
pub struct Destination {
    pub app_id: String,
    pub tenant_id: String,
    pub conversation_id: String,
    pub recipient: Sender,
    pub service_url: String,
    pub activity: Value,
}
impl Destination {
    pub fn from_source(source: &Message, app: &str, tenant: &str) -> Result<Self, &'static str> {
        let data = &source.metadata;
        let route = data.get("continuation_activity").filter(|v| v.is_object());
        if source.channel != "teams"
            || data.get("app_id").and_then(Value::as_str) != Some(app)
            || data.get("tenant_id").and_then(Value::as_str) != Some(tenant)
            || route.is_none()
        {
            return Err("Source does not belong to this Teams adapter");
        }
        let route = route.unwrap();
        let conversation = route
            .get("conversation")
            .and_then(|v| v.get("id"))
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty());
        let recipient = route
            .get("recipient")
            .and_then(|v| v.get("id"))
            .and_then(Value::as_str);
        if conversation.is_none()
            || !recipient.is_some_and(|id| id == app || id == format!("28:{app}"))
            || route
                .get("channelData")
                .and_then(|v| v.get("tenant"))
                .and_then(|v| v.get("id"))
                .and_then(Value::as_str)
                != Some(tenant)
        {
            return Err("Invalid Teams conversation reference");
        }
        Ok(Self {
            app_id: app.into(),
            tenant_id: tenant.into(),
            conversation_id: conversation.unwrap().into(),
            recipient: Sender {
                id: recipient.unwrap().into(),
                display_name: route
                    .get("recipient")
                    .and_then(|v| v.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            },
            service_url: route
                .get("serviceUrl")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
            activity: route.clone(),
        })
    }
    pub fn confirmed_reply(
        &self,
        source: &Message,
        content: String,
        receipt: &str,
    ) -> Result<Message, &'static str> {
        if receipt.is_empty() {
            return Err("Teams did not return a message receipt");
        }
        let mut result = source.clone();
        result.id = format!(
            "teams:{}:{}:{}:{receipt}",
            self.app_id,
            self.tenant_id,
            quote(&self.conversation_id)
        )
        .into();
        result.sender = self.recipient.clone();
        result.text = content;
        result.role = Role::Assistant;
        result.addressed = true;
        result.attachments.clear();
        result.metadata.insert("activity_id".into(), receipt.into());
        Ok(result)
    }
}
/// Least seconds between streamed updates; Teams throttles streams to one request
/// per second.
pub const STREAM_INTERVAL: f64 = 1.5;
/// Longest streamed message before the answer continues in a new one.
pub const STREAM_LIMIT: usize = 20_000;

pub fn text_activity(text: &str) -> Value {
    json!({"type": "message", "text": text, "textFormat": "plain"})
}

/// Teams shows a blank streamed update as nothing; show an ellipsis instead.
fn shown(text: &str) -> &str {
    if text.trim().is_empty() {
        "…"
    } else {
        text
    }
}

/// Teams streams natively only in one-to-one chats; elsewhere a message is updated.
pub fn streams_natively(destination: &Destination) -> bool {
    destination.activity["conversation"]["conversationType"] == "personal"
}

/// The first activity of a streamed answer.
pub fn stream_start_activity(text: &str, native: bool) -> Value {
    if native {
        json!({"type": "typing", "text": shown(text), "entities": [
            {"type": "streaminfo", "streamType": "streaming", "streamSequence": 1}]})
    } else {
        text_activity(shown(text))
    }
}

/// The handle for a stream started with `stream_start_activity` and confirmed as `id`.
pub fn stream_handle(id: &str, native: bool) -> Value {
    json!({"id": id, "native": native, "sequence": 1})
}

/// The next update of a stream, numbering native updates from 1 without gaps.
/// Returns the activity, whether it updates an existing message, and the next handle.
pub fn stream_update_activity(handle: &Value, text: &str) -> (Value, bool, Value) {
    let mut next = handle.clone();
    let id = handle["id"].clone();
    if handle["native"] == true {
        let sequence = handle["sequence"].as_u64().unwrap_or(1) + 1;
        next["sequence"] = json!(sequence);
        let activity = json!({"type": "typing", "id": id, "text": shown(text), "entities": [
            {"type": "streaminfo", "streamId": id, "streamType": "streaming",
             "streamSequence": sequence}]});
        (activity, false, next)
    } else {
        let mut activity = text_activity(shown(text));
        activity["id"] = id;
        (activity, true, next)
    }
}

/// The final message of a stream; a native stream's final message has no sequence.
/// Returns the activity and whether it updates an existing message.
pub fn stream_final_activity(handle: &Value, text: &str) -> (Value, bool) {
    let id = handle["id"].clone();
    let mut activity = text_activity(text);
    activity["id"] = id.clone();
    let native = handle["native"] == true;
    if native {
        activity["entities"] =
            json!([{"type": "streaminfo", "streamId": id, "streamType": "final"}]);
    }
    (activity, !native)
}

/// Whether an activity is for this app in this tenant, from Teams.
pub fn in_tenant(activity: &Value, app_id: &str, tenant_id: &str) -> bool {
    let recipient = activity["recipient"]["id"].as_str().unwrap_or_default();
    activity["channelId"] == "msteams"
        && activity["channelData"]["tenant"]["id"] == tenant_id
        && (recipient == app_id || recipient == format!("28:{app_id}"))
        && activity["from"]["role"] != "bot"
}

/// Where an activity goes when the application has a handler for it:
/// - an `adaptiveCard/action` invoke is an action by its `verb`;
/// - a `task/fetch` invoke (a button opening a dialog) is an action by the
///   button data's `action_id`, and `task/submit` a view by its `callback_id`;
/// - an `Action.Submit` press whose data has an `action_id` is an action;
/// - a message starting with `/name` is a command;
/// - any other activity type is an event, such as `conversationUpdate`.
pub fn route(activity: &Value) -> Option<agentchat_core::Route> {
    use agentchat_core::Route;
    let text = |value: &Value| value.as_str().filter(|v| !v.is_empty()).map(str::to_owned);
    match activity["type"].as_str()? {
        "invoke" => Some(match activity["name"].as_str().unwrap_or_default() {
            "adaptiveCard/action" => match text(&activity["value"]["action"]["verb"]) {
                Some(verb) => Route::Action {
                    action_ids: vec![verb],
                },
                None => Route::Other,
            },
            "task/fetch" => match text(&activity["value"]["data"]["action_id"]) {
                Some(id) => Route::Action {
                    action_ids: vec![id],
                },
                None => Route::Other,
            },
            "task/submit" => match text(&activity["value"]["data"]["callback_id"]) {
                Some(id) => Route::View { callback_id: id },
                None => Route::Other,
            },
            _ => Route::Other,
        }),
        "message" => {
            if activity["value"]["agentchat"].is_string() {
                return None;
            }
            if let Some(id) = text(&activity["value"]["action_id"]) {
                return Some(Route::Action {
                    action_ids: vec![id],
                });
            }
            let raw = activity["text"].as_str()?;
            let without_mentions = MENTION.replace_all(raw, "");
            agentchat_core::command_of(&without_mentions).map(|command| Route::Command { command })
        }
        kind => Some(Route::Event {
            event_type: kind.into(),
        }),
    }
}

static MENTION: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?s)<at>.*?</at>").expect("static Teams mention expression")
});

/// The `invokeResponse` activity carrying the handler's answer, such as a dialog
/// (`{"task": {"type": "continue", "value": ...}}`) or a refreshed card.
pub fn invoke_response(body: &Value) -> Value {
    let mut response = json!({"status": 200});
    if !body.is_null() {
        response["body"] = body.clone();
    }
    json!({"type": "invokeResponse", "value": response})
}

/// A targeted message: shown in the conversation only to `user_id` ("Only you can
/// see this message"); it expires after 24 hours.
pub fn targeted_activity(text: &str, user_id: &str, attachments: Option<&Value>) -> Value {
    let mut activity = text_activity(text);
    activity["recipient"] = json!({"id": user_id, "role": "user"});
    activity["entities"] = json!([{"type": "activityTreatment", "treatment": "targeted"}]);
    if let Some(attachments) = attachments.filter(|a| a.as_array().is_some_and(|a| !a.is_empty())) {
        activity["attachments"] = attachments.clone();
    }
    activity
}

/// The line an informative stream update shows: the newest task in progress,
/// else the plan, else the newest task. Teams allows 1,000 characters.
pub fn informative_text(board: &agentchat_core::TaskBoard) -> String {
    let line = board
        .tasks
        .iter()
        .rev()
        .find(|task| task.status == agentchat_core::TaskStatus::InProgress)
        .map(|task| task.title.clone())
        .or_else(|| board.plan.clone())
        .or_else(|| board.tasks.last().map(|task| task.title.clone()))
        .unwrap_or_default();
    line.trim().chars().take(1000).collect()
}

/// An informative update of a native stream: the first one starts the stream.
/// Returns the activity and the next handle.
pub fn informative_activity(handle: Option<&Value>, text: &str) -> (Value, Value) {
    match handle {
        None => (
            json!({"type": "typing", "text": text, "entities": [
                {"type": "streaminfo", "streamType": "informative", "streamSequence": 1}]}),
            json!({"native": true, "sequence": 1}),
        ),
        Some(handle) => {
            let mut next = handle.clone();
            let sequence = handle["sequence"].as_u64().unwrap_or(1) + 1;
            next["sequence"] = json!(sequence);
            let id = handle["id"].clone();
            (
                json!({"type": "typing", "id": id, "text": text, "entities": [
                    {"type": "streaminfo", "streamId": id, "streamType": "informative",
                     "streamSequence": sequence}]}),
                next,
            )
        }
    }
}

/// Whether a failed stream update means the person pressed Teams' Stop button.
pub fn stream_canceled(status: u16, body: &str) -> bool {
    status == 403
        && body.contains("ContentStreamNotAllowed")
        && body.to_lowercase().contains("canceled by user")
}

/// A stop request for the conversation of `source`, from its sender: Teams' Stop
/// button on a streamed answer in a one-to-one chat.
pub fn stop_request(source: &Message, stream_id: &str) -> Message {
    let mut stop = source.clone();
    stop.id = format!("{}:stop:{stream_id}", source.id).into();
    stop.text = String::new();
    stop.attachments.clear();
    stop.metadata
        .insert(agentchat_core::STOP_METADATA.into(), json!(true));
    stop
}

#[cfg(test)]
mod tests {
    use agentchat_core::Route;

    fn activity(kind: &str) -> Value {
        json!({"type": kind, "channelId": "msteams", "recipient": {"id": "28:app"},
               "from": {"id": "29:user"}, "channelData": {"tenant": {"id": "tenant"}}})
    }

    #[test]
    fn activities_route_by_type_and_invoke_name() {
        let mut action = activity("invoke");
        action["name"] = json!("adaptiveCard/action");
        action["value"] = json!({"action": {"verb": "approve"}});
        assert_eq!(
            route(&action),
            Some(Route::Action {
                action_ids: vec!["approve".into()]
            })
        );
        let mut fetch = activity("invoke");
        fetch["name"] = json!("task/fetch");
        fetch["value"] = json!({"data": {"action_id": "settings"}});
        assert_eq!(
            route(&fetch),
            Some(Route::Action {
                action_ids: vec!["settings".into()]
            })
        );
        let mut submit = activity("invoke");
        submit["name"] = json!("task/submit");
        submit["value"] = json!({"data": {"callback_id": "settings"}});
        assert_eq!(
            route(&submit),
            Some(Route::View {
                callback_id: "settings".into()
            })
        );
        let mut command = activity("message");
        command["text"] = json!("<at>Bot</at> /deploy api");
        assert_eq!(
            route(&command),
            Some(Route::Command {
                command: "/deploy".into()
            })
        );
        let mut prompt = activity("message");
        prompt["value"] = json!({"agentchat": "ac:x:y"});
        assert_eq!(route(&prompt), None);
        assert_eq!(
            route(&activity("conversationUpdate")),
            Some(Route::Event {
                event_type: "conversationUpdate".into()
            })
        );
        assert!(in_tenant(&activity("message"), "app", "tenant"));
        assert!(!in_tenant(&activity("message"), "app", "other"));
        assert_eq!(
            invoke_response(&json!({"task": {}}))["value"]["body"],
            json!({"task": {}})
        );
        let targeted = targeted_activity("hi", "29:user", None);
        assert_eq!(targeted["entities"][0]["treatment"], "targeted");
        assert_eq!(targeted["recipient"]["id"], "29:user");
        let (start, handle) = informative_activity(None, "Searching");
        assert_eq!(start["entities"][0]["streamType"], "informative");
        let (next, _) = informative_activity(
            Some(&json!({"id": "s1", "sequence": handle["sequence"]})),
            "Reading",
        );
        assert_eq!(next["entities"][0]["streamSequence"], 2);
        assert!(stream_canceled(403, "{\"code\":\"ContentStreamNotAllowed\",\"message\":\"Content stream was canceled by user.\"}"));
        assert!(!stream_canceled(
            403,
            "ContentStreamNotAllowed: message size too large"
        ));
    }

    use super::*;

    #[test]
    fn native_streams_number_updates_and_end_without_a_sequence() {
        let start = stream_start_activity(" ", true);
        assert_eq!(start["text"], "…");
        assert_eq!(start["entities"][0]["streamSequence"], 1);
        let handle = stream_handle("A1", true);
        let (update, edits, handle) = stream_update_activity(&handle, "Hel");
        assert!(!edits);
        assert_eq!(update["entities"][0]["streamSequence"], 2);
        assert_eq!(update["entities"][0]["streamId"], "A1");
        let (_, _, handle) = stream_update_activity(&handle, "Hello");
        assert_eq!(handle["sequence"], 3);
        let (last, edits) = stream_final_activity(&handle, "Hello!");
        assert!(!edits);
        assert_eq!(last["type"], "message");
        assert_eq!(last["entities"][0]["streamType"], "final");
        assert!(last["entities"][0].get("streamSequence").is_none());
        let (update, edits, _) = stream_update_activity(&stream_handle("A2", false), "Hi");
        assert!(edits);
        assert_eq!(
            (update["type"].as_str(), update["id"].as_str()),
            (Some("message"), Some("A2"))
        );
    }
    #[test]
    fn enforce_bytes() {
        assert!(text_payload(&"😀".repeat(6000)).is_ok());
        assert!(text_payload(&"😀".repeat(6001)).is_err());
    }
    #[test]
    fn validate_before_connector() {
        assert!(validate_service_url("https://smba.trafficmanager.net/amer/", true).is_ok());
        assert!(validate_service_url("http://smba.trafficmanager.net/amer/", true).is_err());
        assert!(validate_service_url("https://evil.test/", false).is_err());
    }

    #[test]
    fn thread_roots_and_tenant_identity_survive_continuations() {
        let event = json!({
            "type": "message",
            "channelId": "msteams",
            "id": "first",
            "serviceUrl": "https://smba.trafficmanager.net/amer/",
            "conversation": {
                "id": "conversation",
                "conversationType": "channel",
            },
            "from": {"id": "alice", "name": "雪"},
            "recipient": {"id": "28:app", "name": "Bot"},
            "channelData": {"tenant": {"id": "tenant"}},
            "text": "<at>Bot</at> hello",
            "entities": [{
                "type": "mention",
                "text": "<at>Bot</at>",
                "mentioned": {"id": "28:app"},
            }],
        });
        let first = normalize(&event, "app", "tenant").unwrap();
        let mut event = event;
        event["id"] = "second".into();
        event["conversation"]["id"] = "conversation;messageid=first".into();
        let second = normalize(&event, "app", "tenant").unwrap();
        assert_eq!(first.conversation_id, second.conversation_id);
        assert_eq!(second.thread_id.as_deref(), Some("first"));
        assert_eq!(second.text, "hello");
        let route = Destination::from_source(&second, "app", "tenant").unwrap();
        assert_eq!(route.activity["id"], "first");
        let response = route
            .confirmed_reply(&second, "answer".into(), "sent")
            .unwrap();
        assert_eq!(response.thread_id, second.thread_id);
        assert_eq!(response.sender.id, "28:app");
        assert!(Destination::from_source(&second, "app", "other").is_err());
        assert!(normalize(&event, "app", "other").is_none());
    }
}
