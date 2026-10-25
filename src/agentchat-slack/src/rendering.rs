//! Provider-owned formatting, preserving Unicode character boundaries.

use agentchat_core::{Error, Message, Result, RichReply, Role, Sender};
use serde_json::{json, Map, Value};

pub const MIRROR_EVENT_TYPE: &str = "agentchat_user_mirror_v1";

/// A prompt as a text section and one button per choice; buttons carry the token.
pub fn prompt_blocks(
    text: &str,
    prompt: &str,
    choices: &[(String, String, String)],
) -> Result<Vec<Value>> {
    let mut blocks = text_blocks(text)?;
    let buttons = choices
        .iter()
        .map(|(id, label, style)| {
            let mut button = json!({
                "type": "button",
                "action_id": format!("agentchat_{id}"),
                "text": {"type": "plain_text", "text": label, "emoji": true},
                "value": agentchat_core::prompt_token(prompt, id)?,
            });
            if style == "primary" || style == "danger" {
                button["style"] = json!(style);
            }
            Ok(button)
        })
        .collect::<Result<Vec<_>>>()?;
    blocks.push(
        json!({"type": "actions", "block_id": format!("agentchat_{prompt}"), "elements": buttons}),
    );
    Ok(blocks)
}

/// Split a single Slack message into expanded sections without breaking code fences.
pub fn text_blocks(text: &str) -> Result<Vec<Value>> {
    if super::events::trim_python(text).is_empty() {
        return Err(Error::InvalidInput(
            "Slack replies require readable text".into(),
        ));
    }
    let chars: Vec<char> = text.chars().collect();
    let mut rest = chars.as_slice();
    let mut opened = false;
    let mut blocks = Vec::new();
    while !rest.is_empty() {
        let limit = 2992;
        let mut end = if rest.len() <= limit {
            rest.len()
        } else {
            rest[..limit]
                .iter()
                .rposition(|c| *c == '\n')
                .map_or(0, |i| i + 1)
        };
        if end < limit / 2 && rest.len() > limit {
            end = limit;
        }
        for offset in [2, 1] {
            if end >= offset
                && rest.get(end - offset..end - offset + 3) == Some(&['`', '`', '`'][..])
            {
                end -= offset;
                break;
            }
        }
        let part: String = rest[..end].iter().collect();
        let in_fence = opened ^ (part.matches("```").count() % 2 == 1);
        let body = format!(
            "{}{}{}",
            if opened { "```\n" } else { "" },
            part,
            if in_fence { "\n```" } else { "" }
        );
        blocks.push(json!({"type":"section", "expand":true,
            "text":{"type":"mrkdwn", "text":body, "verbatim":true}}));
        if blocks.len() > 50 {
            return Err(Error::InvalidInput(
                "Slack replies support at most 50 sections; split the reply".into(),
            ));
        }
        rest = &rest[end..];
        opened = in_fence;
    }
    Ok(blocks)
}

fn expanded_blocks(blocks: &[Map<String, Value>]) -> Vec<Value> {
    blocks
        .iter()
        .map(|block| {
            let mut copied = block.clone();
            if copied.get("type").is_some_and(|v| v == "section") {
                copied.entry("expand").or_insert(Value::Bool(true));
            }
            Value::Object(copied)
        })
        .collect()
}

pub fn rich_payload(reply: &RichReply) -> Result<Value> {
    rich_payload_with_fallback_blocks(reply, true)
}

pub(crate) fn rich_payload_with_fallback_blocks(
    reply: &RichReply,
    fallback_blocks: bool,
) -> Result<Value> {
    if super::events::trim_python(&reply.text).is_empty() {
        return Err(Error::InvalidInput(
            "Rich replies require fallback text".into(),
        ));
    }
    let blocks = if reply.blocks.is_empty() && fallback_blocks {
        text_blocks(&reply.text)?
    } else {
        expanded_blocks(&reply.blocks)
    };
    let mut payload = json!({"text":reply.text, "blocks":blocks,
        "unfurl_links":false,"unfurl_media":false,"parse":"none","link_names":false});
    if !reply.attachments.is_empty() {
        let attachments: Result<Vec<_>> = reply
            .attachments
            .iter()
            .map(|attachment| {
                let mut attachment = attachment.clone();
                if let Some(blocks) = attachment.get("blocks") {
                    let blocks = blocks.as_array().ok_or_else(|| {
                        Error::InvalidInput("Attachment blocks must be an array".into())
                    })?;
                    let blocks = blocks
                        .iter()
                        .map(|block| {
                            block.as_object().cloned().ok_or_else(|| {
                                Error::InvalidInput("Slack blocks must be objects".into())
                            })
                        })
                        .collect::<Result<Vec<_>>>()?;
                    attachment.insert("blocks".into(), Value::Array(expanded_blocks(&blocks)));
                }
                Ok(Value::Object(attachment))
            })
            .collect();
        payload["attachments"] = Value::Array(attachments?);
    }
    Ok(payload)
}

pub fn mirror_payload(message: &Message, origin: &str) -> Result<Value> {
    if message.role != Role::User
        || message.id.as_ref().is_empty()
        || message.sender.id.is_empty()
        || message.id.as_ref().chars().count() > 256
        || message.sender.id.chars().count() > 256
    {
        return Err(Error::InvalidInput(
            "A mirror requires a user input with stable IDs of at most 256 characters".into(),
        ));
    }
    if message.text.is_empty() || message.text.chars().count() > 2800 {
        return Err(Error::InvalidInput(
            "Mirror text must contain 1-2800 characters".into(),
        ));
    }
    if origin.trim().is_empty() || origin.chars().count() > 64 {
        return Err(Error::InvalidInput(
            "Mirror origin must contain 1-64 characters".into(),
        ));
    }
    let name: String = message
        .sender
        .display_name
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(&message.sender.id)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(160)
        .collect();
    let origin = origin.split_whitespace().collect::<Vec<_>>().join(" ");
    let label = format!("{name} · via {origin}");
    let text = format!("{label}\n{}", message.text)
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    Ok(json!({"text":text,"blocks":[
        {"type":"context","elements":[{"type":"plain_text","text":label,"emoji":false}]},
        {"type":"section","block_id":"agentchat_mirror_body","text":{"type":"plain_text","text":message.text,"emoji":false}}],
        "metadata":{"event_type":MIRROR_EVENT_TYPE,"event_payload":{
            "message_id":message.id,"sender_id":message.sender.id,"sender_name":name,"origin":origin}},
        "mrkdwn":false,"parse":"none","link_names":false,"unfurl_links":false,"unfurl_media":false}))
}

pub struct MirroredInput {
    pub sender: Sender,
    pub text: String,
}

pub fn read_mirror(record: &Value, bot_user_id: &str) -> Option<MirroredInput> {
    if record["user"] != bot_user_id || record["metadata"]["event_type"] != MIRROR_EVENT_TYPE {
        return None;
    }
    let payload = &record["metadata"]["event_payload"];
    for key in ["message_id", "sender_id", "sender_name", "origin"] {
        super::events::nonempty(&payload[key])?;
    }
    let body = record["blocks"].as_array()?.iter().find(|block| {
        block["block_id"] == "agentchat_mirror_body" && block["text"]["type"] == "plain_text"
    })?;
    Some(MirroredInput {
        sender: Sender {
            id: payload["sender_id"].as_str()?.into(),
            display_name: Some(payload["sender_name"].as_str()?.into()),
        },
        text: body["text"]["text"].as_str()?.into(),
    })
}
