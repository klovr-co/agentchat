//! Pure Slack event normalization. No authentication, downloads, or network I/O.

use agentchat_core::{Attachment, Message, Role, Sender};
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::OnceLock;
use unicode_general_category::{get_general_category, GeneralCategory};

pub(crate) fn nonempty(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| !text.is_empty())
}

pub(crate) fn trim_python(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || matches!(c, '\u{001c}'..='\u{001f}'))
}

#[derive(Clone, Debug, Serialize)]
pub struct RoutedText {
    pub text: String,
    pub mentioned: bool,
}

/// Code and quoted examples are source text, not routing instructions.
pub fn routing_text(text: &str, bot_user_id: Option<&str>) -> RoutedText {
    static QUOTED: OnceLock<Regex> = OnceLock::new();
    let quoted = QUOTED.get_or_init(|| {
        Regex::new(r"(?m)```[\s\S]*?(?:```|\z)|`[^`\n]*(?:`|$)|^[ \t]*>>>[\s\S]*|^[ \t]*>[^\n]*")
            .expect("static Slack quote expression")
    });
    let mention = bot_user_id
        .filter(|id| !id.is_empty())
        .map(|id| format!("<@{id}>"));
    let mut result = String::new();
    let mut mentioned = false;
    let mut end = 0;
    for span in quoted.find_iter(text) {
        let plain = &text[end..span.start()];
        if let Some(ref token) = mention {
            mentioned |= plain.contains(token);
            result.push_str(&plain.replace(token, ""));
        } else {
            result.push_str(plain);
        }
        result.push_str(span.as_str());
        end = span.end();
    }
    let plain = &text[end..];
    if let Some(ref token) = mention {
        mentioned |= plain.contains(token);
        result.push_str(&plain.replace(token, ""));
    } else {
        result.push_str(plain);
    }
    RoutedText {
        text: trim_python(&result).into(),
        mentioned,
    }
}

/// Preserve provider previews as reference data, never instructions or downloads.
pub fn reference(record: &Value, current: bool) -> Value {
    let mut source = serde_json::Map::new();
    let fields: &[&str] = if current {
        &["attachments", "blocks"]
    } else {
        &["text", "attachments", "blocks", "files"]
    };
    for key in fields {
        if let Some(value) = record.get(*key).filter(|v| !v.is_null()) {
            source.insert((*key).into(), value.clone());
        }
    }
    if current {
        if let Some(Value::Array(blocks)) = source.get_mut("blocks") {
            blocks.retain(|block| block["type"] != "rich_text");
            if blocks.is_empty() {
                source.remove("blocks");
            }
        }
    }
    Value::Object(source)
}

pub(crate) struct HistoryRecord {
    pub timestamp: super::route::SlackTimestamp,
    pub sender: Sender,
    pub role: Role,
    pub text: String,
    pub files: Vec<Attachment>,
    pub reference: Value,
}

/// Shared by transport history and host context; speaker/mirror semantics have one owner.
pub(crate) fn history_record(record: &Value, bot_user_id: Option<&str>) -> Option<HistoryRecord> {
    let timestamp = record["ts"]
        .as_str()
        .and_then(super::route::SlackTimestamp::parse)?;
    let sender = nonempty(&record["user"]).or_else(|| nonempty(&record["bot_id"]))?;
    let text = record.get("text").map_or(Some(""), Value::as_str)?;
    let files = attachments(&record["files"]);
    let source = reference(record, false);
    if trim_python(text).is_empty()
        && files.is_empty()
        && record["attachments"].is_null()
        && record["blocks"].is_null()
    {
        return None;
    }
    let mirror = bot_user_id.and_then(|bot| super::rendering::read_mirror(record, bot));
    let role = if Some(sender) == bot_user_id && mirror.is_none() {
        Role::Assistant
    } else {
        Role::User
    };
    let input = mirror.unwrap_or_else(|| super::rendering::MirroredInput {
        sender: Sender {
            id: sender.into(),
            display_name: (Some(sender) == bot_user_id)
                .then(|| nonempty(&record["username"]).map(str::to_owned))
                .flatten(),
        },
        text: text.into(),
    });
    Some(HistoryRecord {
        timestamp,
        sender: input.sender,
        role,
        text: input.text,
        files,
        reference: source,
    })
}

pub fn valid_file_id(id: &str) -> bool {
    (2..=31).contains(&id.len())
        && id.starts_with('F')
        && id
            .bytes()
            .skip(1)
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

pub fn filename(value: &str) -> String {
    let normalized = value.replace('\\', "/");
    let name: String = normalized
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| {
            !matches!(
                get_general_category(*c),
                GeneralCategory::Control
                    | GeneralCategory::Format
                    | GeneralCategory::Surrogate
                    | GeneralCategory::PrivateUse
                    | GeneralCategory::Unassigned
            )
        })
        .collect();
    let name: String = name.trim_matches([' ', '.']).chars().take(180).collect();
    if name.is_empty() {
        "attachment".into()
    } else {
        name
    }
}

/// Normalize at most five file references. Event URLs and credentials are discarded.
pub fn attachments(files: &Value) -> Vec<Attachment> {
    attachments_bounded(files, 5)
}

pub fn attachments_bounded(files: &Value, limit: usize) -> Vec<Attachment> {
    let Some(files) = files.as_array() else {
        return Vec::new();
    };
    let mut selected: Vec<Attachment> = Vec::new();
    for file in files.iter().take(limit) {
        let Some(id) = nonempty(&file["id"]).filter(|id| valid_file_id(id)) else {
            continue;
        };
        if selected.iter().any(|item| item.id == id) {
            continue;
        }
        selected.push(Attachment {
            id: id.into(),
            name: file["name"].as_str().map(filename),
            media_type: file["mimetype"]
                .as_str()
                .map(|v| v.chars().take(255).collect()),
            size: file["size"].as_u64(),
        });
    }
    selected
}

/// A press on a prompt button (`block_actions`), as a message in the conversation
/// the prompt was shown in; None for any other interaction.
pub fn normalize_action(
    payload: &Value,
    bot_user_id: Option<&str>,
    workspace_id: Option<&str>,
    channel_name: &str,
) -> Option<Message> {
    if payload["type"] != "block_actions" {
        return None;
    }
    let team = nonempty(&payload["team"]["id"])?;
    if workspace_id.is_some_and(|workspace| team != workspace) {
        return None;
    }
    let user = nonempty(&payload["user"]["id"])?;
    if Some(user) == bot_user_id {
        return None;
    }
    let channel = nonempty(&payload["channel"]["id"])
        .or_else(|| nonempty(&payload["container"]["channel_id"]))?;
    let prompt_ts = nonempty(&payload["message"]["ts"])
        .or_else(|| nonempty(&payload["container"]["message_ts"]))?;
    let thread = nonempty(&payload["message"]["thread_ts"])
        .or_else(|| nonempty(&payload["container"]["thread_ts"]));
    let (action, answer) = payload["actions"].as_array()?.iter().find_map(|action| {
        let answer = agentchat_core::answer_metadata(action["value"].as_str()?)?;
        Some((action, answer))
    })?;
    let action_ts = nonempty(&action["action_ts"]).unwrap_or(prompt_ts);
    let direct = channel.starts_with('D');
    let root = thread.unwrap_or(prompt_ts);
    let mut metadata = json!({
        "channel_id": channel, "team_id": team,
        "channel_type": if direct { "im" } else { "channel" },
        "reply_thread_timestamp": thread, "message_timestamp": prompt_ts,
    });
    metadata[agentchat_core::ANSWER_METADATA] = answer;
    Some(Message {
        id: format!("slack:{team}:{channel}:action:{action_ts}:{user}").into(),
        conversation_id: if direct {
            format!("slack:{team}:{channel}")
        } else {
            format!("slack:{team}:{channel}:{root}")
        }
        .into(),
        channel: channel_name.into(),
        sender: Sender {
            id: user.into(),
            display_name: payload["user"]["name"].as_str().map(str::to_owned),
        },
        text: action["text"]["text"].as_str().unwrap_or_default().into(),
        role: Role::User,
        thread_id: thread.map(str::to_owned),
        addressed: true,
        attachments: vec![],
        metadata: metadata.as_object()?.clone(),
    })
}

/// Decode an authenticated Events API payload without doing I/O.
/// Untagged thread replies are marked for the adapter's subscription check.
pub fn normalize(payload: &Value, bot_user_id: &str, workspace_id: &str) -> Option<Message> {
    normalize_scoped(payload, Some(bot_user_id), Some(workspace_id), "slack")
}

/// Host-owned ingress may identify the bot or workspace after authenticating.
pub fn normalize_scoped(
    payload: &Value,
    bot_user_id: Option<&str>,
    workspace_id: Option<&str>,
    channel_name: &str,
) -> Option<Message> {
    let event = payload.get("event")?.as_object()?;
    let get = |key: &str| event.get(key).unwrap_or(&Value::Null);
    let event_type = get("type").as_str()?;
    let direct = event_type == "message" && get("channel_type") == "im";
    if !get("bot_id").is_null()
        || !get("bot_profile").is_null()
        || (!get("subtype").is_null() && get("subtype") != "file_share")
    {
        return None;
    }
    let user = nonempty(get("user"))?;
    if Some(user) == bot_user_id {
        return None;
    }
    let channel = nonempty(get("channel"))?;
    let timestamp = nonempty(get("ts"))?;
    let text = event.get("text").map_or(Some(""), Value::as_str)?;
    let files = attachments(get("files"));
    if trim_python(text).is_empty() && files.is_empty() {
        return None;
    }
    let team = nonempty(&payload["team_id"])?;
    if workspace_id.is_some_and(|workspace| team != workspace) {
        return None;
    }
    let supplied_thread = if get("thread_ts").is_null() {
        None
    } else {
        Some(nonempty(get("thread_ts"))?)
    };
    let root = supplied_thread.unwrap_or(timestamp);
    let routed = routing_text(text, bot_user_id);
    let channel_message = event_type == "message"
        && matches!(get("channel_type").as_str(), Some("channel" | "group"));
    let mentioned = event_type == "app_mention" || (channel_message && routed.mentioned);
    if !(direct || mentioned || (channel_message && supplied_thread.is_some())) {
        return None;
    }
    let normalized_text = routed.text;
    if normalized_text.is_empty() && files.is_empty() {
        return None;
    }
    let thread = if direct { supplied_thread } else { Some(root) };
    let event_id = payload["event_id"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{channel}:{timestamp}"));
    let mut metadata = json!({
            "channel_id": channel, "team_id": team, "event_id": event_id,
            "channel_type": if direct { Value::String("im".into()) } else { event.get("channel_type").cloned().unwrap_or_else(|| Value::String("channel".into())) },
            "reply_thread_timestamp": thread, "message_timestamp": timestamp,
            "requires_subscription": !direct && !mentioned,
    }).as_object()?.clone();
    let reference = reference(&payload["event"], true);
    if reference
        .as_object()
        .is_some_and(|source| !source.is_empty())
    {
        metadata.insert("slack_reference".into(), reference);
    }
    Some(Message {
        id: format!("slack:{team}:{channel}:{timestamp}").into(),
        conversation_id: if direct {
            format!("slack:{team}:{channel}")
        } else {
            format!("slack:{team}:{channel}:{root}")
        }
        .into(),
        channel: channel_name.into(),
        sender: Sender {
            id: user.into(),
            display_name: None,
        },
        text: normalized_text,
        role: Role::User,
        thread_id: thread.map(str::to_owned),
        addressed: direct || mentioned,
        attachments: files,
        metadata,
    })
}
