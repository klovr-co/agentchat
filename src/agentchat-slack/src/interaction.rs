//! Slack payloads that are not chat messages: events, button presses, view
//! submissions, slash commands and shortcuts, and the requests an application
//! makes in reply to them. Pure policy shared by every transport; no network I/O.
//!
//! AgentChat turns messages, prompt presses and Slack's native stop into its own
//! inputs. Every other authenticated payload is routed to the application's
//! handlers by kind and key. Slack expects an acknowledgement within three
//! seconds; for view submissions, options requests and slash commands that
//! acknowledgement may carry a response body, so transports wait for the handler
//! up to [`ACK_DEADLINE`] before acknowledging.

use std::time::Duration;

use agentchat_core::{Error, Message, Result, Role, Sender, STOP_METADATA};
use serde_json::{json, Map, Value};

use super::events::nonempty;
use super::route::SlackTimestamp;

/// How long a transport waits for a handler whose return value becomes the
/// acknowledgement body. Slack allows three seconds including network time; a
/// handler that misses this is acknowledged without a body and keeps running.
pub const ACK_DEADLINE: Duration = Duration::from_millis(2_500);

pub use agentchat_core::Route;

/// Classify an authenticated payload for the application's handlers.
pub fn route(payload: &Value) -> Route {
    if let Some(event) = payload.get("event").filter(|event| event.is_object()) {
        return Route::Event {
            event_type: event["type"].as_str().unwrap_or_default().to_owned(),
        };
    }
    if let Some(command) = nonempty(&payload["command"]) {
        return Route::Command {
            command: command.to_owned(),
        };
    }
    match payload["type"].as_str() {
        Some("block_actions") => Route::Action {
            action_ids: payload["actions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|action| nonempty(&action["action_id"]).map(str::to_owned))
                .collect(),
        },
        Some("view_submission" | "view_closed") => Route::View {
            callback_id: payload["view"]["callback_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        },
        _ => Route::Other,
    }
}

/// Whether Slack accepts a response body in this payload's acknowledgement: view
/// submissions (`response_action`), options requests and slash commands.
pub fn responds(payload: &Value) -> bool {
    nonempty(&payload["command"]).is_some()
        || matches!(
            payload["type"].as_str(),
            Some("view_submission" | "block_suggestion")
        )
}

/// The workspace a payload belongs to: Events API envelopes and slash commands
/// carry `team_id`, interactive payloads `team.id` (or the user's or view's team).
pub fn team_of(payload: &Value) -> Option<&str> {
    nonempty(&payload["team_id"])
        .or_else(|| nonempty(&payload["team"]["id"]))
        .or_else(|| nonempty(&payload["user"]["team_id"]))
        .or_else(|| nonempty(&payload["view"]["team_id"]))
}

/// Payloads from another workspace are dropped, including ones without a team.
pub fn in_workspace(payload: &Value, workspace_id: Option<&str>) -> bool {
    workspace_id.is_none_or(|workspace| team_of(payload) == Some(workspace))
}

/// Slack's native stop button (`agent_session_stopped`) as a stop request for the
/// conversation of the thread it was pressed in, sent by the person who pressed.
pub fn normalize_stop(
    payload: &Value,
    workspace_id: Option<&str>,
    channel_name: &str,
) -> Option<Message> {
    let event = &payload["event"];
    if event["type"] != "agent_session_stopped" {
        return None;
    }
    let team = nonempty(&payload["team_id"])?;
    if workspace_id.is_some_and(|workspace| team != workspace) {
        return None;
    }
    let channel = nonempty(&event["channel"])?;
    super::policy::validate_channel(channel).ok()?;
    let thread = nonempty(&event["thread_ts"]).filter(|ts| SlackTimestamp::parse(ts).is_some())?;
    let user = nonempty(&event["user"])?;
    let event_ts = nonempty(&event["event_ts"]).unwrap_or(thread);
    let direct = channel.starts_with('D');
    let mut metadata = json!({
        "channel_id": channel, "team_id": team,
        "channel_type": if direct { "im" } else { "channel" },
        "reply_thread_timestamp": thread, "message_timestamp": thread,
        "streaming_message_ts": event.get("streaming_message_ts").cloned().unwrap_or(json!([])),
    });
    metadata[STOP_METADATA] = Value::Bool(true);
    Some(Message {
        id: format!("slack:{team}:{channel}:stop:{event_ts}:{user}").into(),
        conversation_id: if direct {
            format!("slack:{team}:{channel}")
        } else {
            format!("slack:{team}:{channel}:{thread}")
        }
        .into(),
        channel: channel_name.into(),
        sender: Sender {
            id: user.into(),
            display_name: None,
        },
        text: String::new(),
        role: Role::User,
        thread_id: Some(thread.into()),
        addressed: true,
        attachments: vec![],
        metadata: metadata.as_object()?.clone(),
    })
}

/// Validate a Slack user ID once at the provider boundary.
pub fn validate_user(user_id: &str) -> Result<()> {
    if user_id.len() < 2
        || !matches!(user_id.as_bytes()[0], b'U' | b'W')
        || !user_id
            .bytes()
            .skip(1)
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return Err(Error::InvalidInput("Expected a Slack user ID".into()));
    }
    Ok(())
}

/// `chat.postEphemeral` for `user_id` in `channel_id` (and thread, if any).
pub fn ephemeral_request(
    channel_id: &str,
    thread_ts: Option<&str>,
    user_id: &str,
    content: &agentchat_core::RichReply,
) -> Result<Value> {
    super::policy::ReplyDestination::validate(channel_id, thread_ts)?;
    validate_user(user_id)?;
    let mut request = super::rendering::rich_payload(content)?;
    request["channel"] = json!(channel_id);
    request["user"] = json!(user_id);
    if let Some(thread) = thread_ts {
        request["thread_ts"] = json!(thread);
    }
    Ok(request)
}

/// The receipt of an ephemeral notice. Its `message_timestamp` cannot be updated
/// or deleted through the Web API, so the message is marked `ephemeral`.
pub fn ephemeral_message(
    source: &Message,
    channel_id: &str,
    thread_ts: Option<&str>,
    user_id: &str,
    text: &str,
    response: &Value,
) -> Result<Message> {
    let receipt = super::policy::MessageReceipt::confirmed_timestamp(
        &json!({"ok": response["ok"], "ts": response["message_ts"]}),
        super::policy::DeliveryKind::Ephemeral,
    )?;
    let mut message = super::policy::reply_message(
        source,
        super::policy::ReplyMessageRoute {
            channel_id: channel_id.into(),
            thread_timestamp: json!(thread_ts),
            team_id: source
                .metadata
                .get("team_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        text.into(),
        receipt,
        None,
    );
    message
        .metadata
        .insert("ephemeral".into(), Value::Bool(true));
    message
        .metadata
        .insert("recipient_user_id".into(), json!(user_id));
    Ok(message)
}

/// `chat.delete` for a message this app sent, located by its receipt metadata.
pub fn delete_request(message: &Message, workspace_id: Option<&str>) -> Result<Value> {
    let text = |key: &str| {
        message
            .metadata
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    };
    if message.channel != "slack"
        || workspace_id
            .is_some_and(|workspace| text("team_id").is_some_and(|team| team != workspace))
    {
        return Err(Error::InvalidInput(
            "Expected a message in the configured Slack workspace".into(),
        ));
    }
    if message.metadata.get("ephemeral") == Some(&Value::Bool(true)) {
        return Err(Error::InvalidInput(
            "Slack cannot delete an ephemeral message".into(),
        ));
    }
    let channel = text("channel_id")
        .ok_or_else(|| Error::InvalidInput("Expected a Slack message to delete".into()))?;
    super::policy::validate_channel(channel)?;
    let ts = text("message_timestamp")
        .filter(|ts| SlackTimestamp::parse(ts).is_some())
        .ok_or_else(|| Error::InvalidInput("Expected a Slack message to delete".into()))?;
    Ok(json!({"channel": channel, "ts": ts}))
}

/// Which view an update replaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewTarget {
    /// The `id` Slack returned when the view was opened or pushed.
    Id(String),
    /// The application's own `external_id` for the view.
    External(String),
}

fn view_object(view: &Value) -> Result<&Map<String, Value>> {
    view.as_object()
        .filter(|view| view.get("type").and_then(Value::as_str).is_some())
        .ok_or_else(|| Error::InvalidInput("Expected a Slack view with a type".into()))
}

fn trigger(trigger_id: &str) -> Result<&str> {
    if trigger_id.trim().is_empty() {
        return Err(Error::InvalidInput(
            "Opening a view needs the trigger_id of an interaction".into(),
        ));
    }
    Ok(trigger_id)
}

/// `views.open` (or `views.push` onto an open modal) with an interaction's trigger.
pub fn open_view_request(trigger_id: &str, view: &Value) -> Result<Value> {
    view_object(view)?;
    Ok(json!({"trigger_id": trigger(trigger_id)?, "view": view}))
}

/// `views.update` for the view named by `target`; `hash` guards against races.
pub fn update_view_request(view: &Value, target: &ViewTarget, hash: Option<&str>) -> Result<Value> {
    view_object(view)?;
    let mut request = json!({"view": view});
    match target {
        ViewTarget::Id(id) if !id.trim().is_empty() => request["view_id"] = json!(id),
        ViewTarget::External(id) if !id.trim().is_empty() => request["external_id"] = json!(id),
        _ => {
            return Err(Error::InvalidInput(
                "Updating a view needs its view_id or external_id".into(),
            ))
        }
    }
    if let Some(hash) = hash {
        request["hash"] = json!(hash);
    }
    Ok(request)
}

/// `views.publish` of a user's App Home.
pub fn publish_home_request(user_id: &str, view: &Value, hash: Option<&str>) -> Result<Value> {
    validate_user(user_id)?;
    if view_object(view)?.get("type") != Some(&json!("home")) {
        return Err(Error::InvalidInput(
            "An App Home view must have type \"home\"".into(),
        ));
    }
    let mut request = json!({"user_id": user_id, "view": view});
    if let Some(hash) = hash {
        request["hash"] = json!(hash);
    }
    Ok(request)
}

/// The view in a confirmed `views.*` response.
pub fn view_of(response: &Value) -> Result<Value> {
    if response["ok"] != true || !response["view"].is_object() {
        return Err(Error::Transport("Slack did not return the view".into()));
    }
    Ok(response["view"].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_payloads_by_kind_and_key() {
        let event = json!({"team_id": "T1", "event": {"type": "app_home_opened"}});
        assert_eq!(
            route(&event),
            Route::Event {
                event_type: "app_home_opened".into()
            }
        );
        let press =
            json!({"type": "block_actions", "actions": [{"action_id": "retry"}, {"value": "x"}]});
        assert_eq!(
            route(&press),
            Route::Action {
                action_ids: vec!["retry".into()]
            }
        );
        let view = json!({"type": "view_submission", "view": {"callback_id": "settings"}});
        assert_eq!(
            route(&view),
            Route::View {
                callback_id: "settings".into()
            }
        );
        let command = json!({"command": "/deploy", "team_id": "T1"});
        assert_eq!(
            route(&command),
            Route::Command {
                command: "/deploy".into()
            }
        );
        assert_eq!(route(&json!({"type": "shortcut"})), Route::Other);
        assert!(responds(&view) && responds(&command));
        assert!(responds(&json!({"type": "block_suggestion"})));
        assert!(!responds(&press) && !responds(&event));
        assert!(!responds(&json!({"type": "view_closed"})));
    }

    #[test]
    fn workspace_boundary_fails_closed() {
        let interactive = json!({"type": "block_actions", "team": {"id": "T1"}});
        assert!(in_workspace(&interactive, Some("T1")));
        assert!(!in_workspace(&interactive, Some("T2")));
        assert!(in_workspace(
            &json!({"user": {"team_id": "T1"}}),
            Some("T1")
        ));
        assert!(!in_workspace(&json!({"type": "shortcut"}), Some("T1")));
        assert!(in_workspace(&json!({}), None));
    }

    #[test]
    fn native_stop_targets_the_thread_conversation() {
        let payload = json!({"team_id": "T1", "event": {
            "type": "agent_session_stopped", "channel": "C1", "thread_ts": "1.000001",
            "user": "U1", "event_ts": "2.000001", "streaming_message_ts": ["1.5"],
        }});
        let stop = normalize_stop(&payload, Some("T1"), "slack").unwrap();
        assert!(agentchat_core::is_stop_request(&stop));
        assert_eq!(stop.conversation_id.as_ref(), "slack:T1:C1:1.000001");
        assert_eq!(stop.sender.id, "U1");
        assert!(normalize_stop(&payload, Some("T2"), "slack").is_none());
        let mut direct = payload.clone();
        direct["event"]["channel"] = json!("D1");
        let stop = normalize_stop(&direct, None, "slack").unwrap();
        assert_eq!(stop.conversation_id.as_ref(), "slack:T1:D1");
        let mut bad = payload;
        bad["event"]["thread_ts"] = json!("not-a-ts");
        assert!(normalize_stop(&bad, None, "slack").is_none());
    }

    #[test]
    fn view_requests_validate_their_targets() {
        let modal = json!({"type": "modal", "title": {"type": "plain_text", "text": "Hi"}});
        assert_eq!(
            open_view_request("1.2.abc", &modal).unwrap()["trigger_id"],
            "1.2.abc"
        );
        assert!(open_view_request(" ", &modal).is_err());
        assert!(open_view_request("1", &json!({"blocks": []})).is_err());
        let update =
            update_view_request(&modal, &ViewTarget::External("mine".into()), Some("h")).unwrap();
        assert_eq!(update["external_id"], "mine");
        assert_eq!(update["hash"], "h");
        assert!(update_view_request(&modal, &ViewTarget::Id(String::new()), None).is_err());
        let home = json!({"type": "home", "blocks": []});
        assert_eq!(
            publish_home_request("U1", &home, None).unwrap()["user_id"],
            "U1"
        );
        assert!(publish_home_request("U1", &modal, None).is_err());
        assert!(publish_home_request("bad", &home, None).is_err());
    }

    #[test]
    fn delete_and_ephemeral_requests_never_guess_a_destination() {
        let mut sent = Message {
            id: "slack:C1:1.5".into(),
            conversation_id: "c".into(),
            channel: "slack".into(),
            sender: Sender {
                id: "agentchat".into(),
                display_name: None,
            },
            text: "x".into(),
            role: Role::Assistant,
            metadata: json!({"channel_id": "C1", "message_timestamp": "1.5", "team_id": "T1"})
                .as_object()
                .unwrap()
                .clone(),
            thread_id: None,
            addressed: true,
            attachments: vec![],
        };
        assert_eq!(
            delete_request(&sent, Some("T1")).unwrap(),
            json!({"channel": "C1", "ts": "1.5"})
        );
        assert!(delete_request(&sent, Some("T2")).is_err());
        sent.metadata.insert("ephemeral".into(), json!(true));
        assert!(delete_request(&sent, None).is_err());
        let content = agentchat_core::RichReply {
            text: "Only you".into(),
            blocks: vec![],
            attachments: vec![],
        };
        let request = ephemeral_request("C1", Some("1.0"), "U1", &content).unwrap();
        assert_eq!(request["user"], "U1");
        assert_eq!(request["thread_ts"], "1.0");
        assert!(ephemeral_request("C1", None, "<@U1>", &content).is_err());
    }
}
