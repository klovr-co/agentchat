//! WhatsApp Cloud API normalization, routing, and authentication policy.
use agentchat_core::{Attachment, Message, Role, Sender};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

pub fn validate_config(
    phone: &str,
    token: &str,
    secret: &str,
    verify: &str,
    version: &str,
) -> Result<(), &'static str> {
    if [phone, token, secret, verify]
        .iter()
        .any(|v| v.trim().is_empty())
    {
        return Err("WhatsApp phone_number_id, access_token, app_secret, verify_token required");
    }
    if !digits(phone) {
        return Err("WhatsApp phone_number_id must be the numeric API ID");
    }
    let valid = version
        .strip_prefix('v')
        .and_then(|v| v.split_once('.'))
        .is_some_and(|(major, minor)| digits(major) && !major.starts_with('0') && digits(minor));
    if !valid {
        return Err("WhatsApp api_version must have the form v23.0");
    }
    Ok(())
}
fn digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|c| c.is_ascii_digit())
}
fn items(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}
pub fn normalize(payload: &Value, phone: &str) -> Vec<Message> {
    let mut messages = Vec::new();
    if payload["object"] != "whatsapp_business_account" {
        return messages;
    }
    for entry in items(&payload["entry"]) {
        for change in items(&entry["changes"]) {
            let value = &change["value"];
            if change["field"] != "messages"
                || value["messaging_product"] != "whatsapp"
                || value["metadata"]["phone_number_id"] != phone
            {
                continue;
            }
            for event in items(&value["messages"]) {
                let (Some(sender), Some(id), Some(kind)) = (
                    event["from"].as_str(),
                    event["id"].as_str(),
                    event["type"].as_str(),
                ) else {
                    continue;
                };
                if !digits(sender) || id.is_empty() {
                    continue;
                }
                let mut attachments = Vec::new();
                let empty = json!("");
                let text = match kind {
                    "text" => event["text"].get("body").unwrap_or(&empty),
                    "image" | "audio" | "video" | "document" | "sticker" => {
                        let media = &event[kind];
                        if let Some(id) = media["id"].as_str().filter(|v| !v.is_empty()) {
                            attachments.push(Attachment {
                                id: id.into(),
                                name: media["filename"].as_str().map(str::to_owned),
                                media_type: media["mime_type"].as_str().map(str::to_owned),
                                size: None,
                            });
                        }
                        media.get("caption").unwrap_or(&empty)
                    }
                    "button" => &event["button"]["text"],
                    "interactive" => {
                        let interactive = &event["interactive"];
                        if interactive["button_reply"]
                            .as_object()
                            .is_some_and(|v| !v.is_empty())
                        {
                            &interactive["button_reply"]["title"]
                        } else {
                            &interactive["list_reply"]["title"]
                        }
                    }
                    _ => &Value::Null,
                };
                let Some(text) = text.as_str() else {
                    continue;
                };
                if text.trim().is_empty() && attachments.is_empty() {
                    continue;
                }
                let answer = reply_answer(event);
                let display_name = items(&value["contacts"])
                    .iter()
                    .rev()
                    .find(|v| v["wa_id"] == sender)
                    .and_then(|v| v["profile"]["name"].as_str())
                    .map(str::to_owned);
                messages.push(Message {
                    id: format!("whatsapp:{phone}:{id}").into(),
                    conversation_id: format!("whatsapp:{phone}:{sender}").into(),
                    channel: "whatsapp".into(),
                    sender: Sender {
                        id: sender.into(),
                        display_name,
                    },
                    text: text.into(),
                    role: Role::User,
                    thread_id: None,
                    addressed: true,
                    attachments,
                    metadata: [
                        ("phone_number_id".into(), json!(phone)),
                        ("message_id".into(), json!(id)),
                        ("wa_id".into(), json!(sender)),
                        ("message_type".into(), json!(kind)),
                    ]
                    .into_iter()
                    .chain(answer.map(|a| (agentchat_core::ANSWER_METADATA.into(), a)))
                    .collect(),
                });
            }
        }
    }
    messages
}
pub struct Destination {
    pub recipient: String,
    pub message_id: String,
}
pub fn destination(source: &Message, phone: &str) -> Result<Destination, &'static str> {
    let invalid = "Source does not belong to this WhatsApp adapter";
    if source.channel != "whatsapp" || source.metadata.get("phone_number_id") != Some(&json!(phone))
    {
        return Err(invalid);
    }
    let recipient = source
        .metadata
        .get("wa_id")
        .and_then(Value::as_str)
        .filter(|value| digits(value))
        .ok_or(invalid)?;
    let id = source
        .metadata
        .get("message_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(invalid)?;
    Ok(Destination {
        recipient: recipient.into(),
        message_id: id.into(),
    })
}

pub fn text_payload(
    content: Option<&str>,
    recipient: &str,
    id: &str,
) -> Result<Value, &'static str> {
    let content = content
        .filter(|s| !s.trim().is_empty())
        .ok_or("WhatsApp replies require nonempty text")?;
    if content.chars().count() > 4096 {
        return Err("WhatsApp replies must fit 4,096 characters; send separate replies");
    }
    Ok(
        json!({"messaging_product":"whatsapp","recipient_type":"individual","to":recipient,"type":"text","text":{"body":content,"preview_url":false},"context":{"message_id":id}}),
    )
}
/// Shows "typing…" for the message being answered. WhatsApp also marks that
/// message as read, and clears the indicator after 25 seconds or on the reply.
pub fn typing_payload(message_id: &str) -> Value {
    json!({"messaging_product":"whatsapp","status":"read","message_id":message_id,"typing_indicator":{"type":"text"}})
}
/// The emoji people react with to pick each choice: 👍 for one, 👍 and 👎 for two,
/// keycaps 1️⃣ to 🔟 for more.
pub fn reaction_emojis(count: usize) -> Vec<&'static str> {
    const KEYCAPS: [&str; 10] = [
        "1\u{fe0f}\u{20e3}",
        "2\u{fe0f}\u{20e3}",
        "3\u{fe0f}\u{20e3}",
        "4\u{fe0f}\u{20e3}",
        "5\u{fe0f}\u{20e3}",
        "6\u{fe0f}\u{20e3}",
        "7\u{fe0f}\u{20e3}",
        "8\u{fe0f}\u{20e3}",
        "9\u{fe0f}\u{20e3}",
        "\u{1f51f}",
    ];
    match count {
        1 => vec!["\u{1f44d}"],
        2 => vec!["\u{1f44d}", "\u{1f44e}"],
        _ => KEYCAPS.iter().take(count).copied().collect(),
    }
}
/// Whether two reactions are the same emoji, ignoring skin tones and the
/// variation selector that WhatsApp clients add or drop.
pub fn same_emoji(left: &str, right: &str) -> bool {
    let plain = |s: &str| -> String {
        s.chars()
            .filter(|c| *c != '\u{fe0f}' && !('\u{1f3fb}'..='\u{1f3ff}').contains(c))
            .collect()
    };
    !left.is_empty() && plain(left) == plain(right)
}
/// A prompt as a text reply that lists which emoji to react with for each choice.
/// Returns the payload, the text sent, and each choice's emoji.
pub fn reaction_prompt_payload(
    text: &str,
    recipient: &str,
    id: &str,
    labels: &[String],
) -> Result<(Value, String, Vec<&'static str>), &'static str> {
    let emojis = reaction_emojis(labels.len());
    if emojis.len() != labels.len() {
        return Err("WhatsApp reaction prompts allow 1 to 10 choices");
    }
    let legend = emojis
        .iter()
        .zip(labels)
        .map(|(emoji, label)| format!("{emoji} {label}"))
        .collect::<Vec<_>>()
        .join("\n");
    let body = format!("{}\n\nReact with:\n{legend}", text.trim_end());
    let payload = text_payload(Some(&body), recipient, id)?;
    Ok((payload, body, emojis))
}
/// Reactions to messages, as messages from the person who reacted. Removed
/// reactions (an empty emoji) are left out.
pub fn reactions(payload: &Value, phone: &str) -> Vec<Message> {
    let mut found = Vec::new();
    if payload["object"] != "whatsapp_business_account" {
        return found;
    }
    for entry in items(&payload["entry"]) {
        for change in items(&entry["changes"]) {
            let value = &change["value"];
            if change["field"] != "messages"
                || value["messaging_product"] != "whatsapp"
                || value["metadata"]["phone_number_id"] != phone
            {
                continue;
            }
            for event in items(&value["messages"]) {
                let reaction = &event["reaction"];
                let (Some(sender), Some(id), Some(target), Some(emoji)) = (
                    event["from"].as_str().filter(|s| digits(s)),
                    event["id"].as_str().filter(|s| !s.is_empty()),
                    reaction["message_id"].as_str().filter(|s| !s.is_empty()),
                    reaction["emoji"].as_str().filter(|s| !s.is_empty()),
                ) else {
                    continue;
                };
                if event["type"] != "reaction" {
                    continue;
                }
                found.push(Message {
                    id: format!("whatsapp:{phone}:{id}").into(),
                    conversation_id: format!("whatsapp:{phone}:{sender}").into(),
                    channel: "whatsapp".into(),
                    sender: Sender {
                        id: sender.into(),
                        display_name: items(&value["contacts"])
                            .iter()
                            .rev()
                            .find(|v| v["wa_id"] == sender)
                            .and_then(|v| v["profile"]["name"].as_str())
                            .map(str::to_owned),
                    },
                    text: emoji.into(),
                    role: Role::User,
                    thread_id: None,
                    addressed: true,
                    attachments: vec![],
                    metadata: [
                        ("phone_number_id".into(), json!(phone)),
                        ("message_id".into(), json!(id)),
                        ("wa_id".into(), json!(sender)),
                        ("message_type".into(), json!("reaction")),
                        ("reacted_to".into(), json!(target)),
                        ("emoji".into(), json!(emoji)),
                    ]
                    .into_iter()
                    .collect(),
                });
            }
        }
    }
    found
}
/// The `interactive` object of a prompt: up to three short choices become reply
/// buttons (titles of 20 characters); otherwise a list of up to ten rows, with long
/// labels continued in the row's description. YCloud sends the same object.
pub fn prompt_interactive(
    text: &str,
    prompt: &str,
    choices: &[(String, String)],
) -> Result<Value, &'static str> {
    if text.trim().is_empty() || text.chars().count() > 1024 {
        return Err("WhatsApp prompts need 1 to 1,024 characters of text");
    }
    let token = |choice: &str| {
        agentchat_core::prompt_token(prompt, choice).map_err(|_| "Invalid prompt or choice ID")
    };
    let short = |label: &str, limit: usize| -> String {
        if label.chars().count() <= limit {
            label.into()
        } else {
            label.chars().take(limit - 1).chain(['…']).collect()
        }
    };
    let mut interactive =
        if choices.len() <= 3 && choices.iter().all(|(_, l)| l.chars().count() <= 20) {
            let buttons = choices
                .iter()
                .map(|(choice, label)| {
                    Ok(json!({"type": "reply", "reply": {"id": token(choice)?, "title": label}}))
                })
                .collect::<Result<Vec<_>, &'static str>>()?;
            json!({"type": "button", "action": {"buttons": buttons}})
        } else {
            let rows = choices
                .iter()
                .map(|(choice, label)| {
                    let mut row = json!({"id": token(choice)?, "title": short(label, 24)});
                    if label.chars().count() > 24 {
                        row["description"] = json!(short(label, 72));
                    }
                    Ok(row)
                })
                .collect::<Result<Vec<_>, &'static str>>()?;
            json!({"type": "list", "action": {"button": "Choose", "sections": [{"rows": rows}]}})
        };
    interactive["body"] = json!({"text": text});
    Ok(interactive)
}
/// The answer a button or list reply carries, if it is a reply to a prompt.
pub fn reply_answer(event: &Value) -> Option<Value> {
    let interactive = &event["interactive"];
    let reply = if interactive["button_reply"].is_object() {
        &interactive["button_reply"]
    } else {
        &interactive["list_reply"]
    };
    (event["type"] == "interactive")
        .then(|| reply["id"].as_str())
        .flatten()
        .and_then(agentchat_core::answer_metadata)
}
/// A prompt as a Cloud API interactive reply to the source message.
pub fn prompt_payload(
    text: &str,
    recipient: &str,
    id: &str,
    prompt: &str,
    choices: &[(String, String)],
) -> Result<Value, &'static str> {
    Ok(json!({
        "messaging_product": "whatsapp",
        "recipient_type": "individual",
        "to": recipient,
        "type": "interactive",
        "interactive": prompt_interactive(text, prompt, choices)?,
        "context": {"message_id": id},
    }))
}
pub fn receipt(
    source: &Message,
    phone: &str,
    content: &str,
    result: &Value,
) -> Result<Message, &'static str> {
    let receipts = items(&result["messages"]);
    let id = if receipts.len() == 1 {
        receipts[0]["id"].as_str().filter(|s| !s.is_empty())
    } else {
        None
    }
    .ok_or("WhatsApp did not return a message receipt")?;
    let mut sent = source.clone();
    sent.id = format!("whatsapp:{phone}:{id}").into();
    sent.sender = Sender {
        id: phone.into(),
        display_name: None,
    };
    sent.text = content.into();
    sent.role = Role::Assistant;
    sent.addressed = true;
    sent.thread_id = None;
    sent.attachments.clear();
    sent.metadata.insert("message_id".into(), json!(id));
    Ok(sent)
}
pub fn verify_signature(secret: &str, raw: &[u8], supplied: &str) -> bool {
    let Some(signature) = supplied
        .strip_prefix("sha256=")
        .and_then(|v| hex::decode(v).ok())
    else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(raw);
    mac.verify_slice(&signature).is_ok()
}
pub fn verify_challenge(mode: &str, challenge: &str, supplied: &str, expected: &str) -> bool {
    mode == "subscribe"
        && !challenge.is_empty()
        && constant_time_eq(supplied.as_bytes(), expected.as_bytes())
}
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(b"AgentChat constant time comparison") else {
        return false;
    };
    mac.update(left);
    let digest = mac.finalize().into_bytes();
    let Ok(mut check) = Hmac::<Sha256>::new_from_slice(b"AgentChat constant time comparison")
    else {
        return false;
    };
    check.update(right);
    check.verify_slice(&digest).is_ok()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validation_and_payload() {
        assert!(validate_config("123", "t", "s", "v", "v23.0").is_ok());
        assert!(validate_config("١", "t", "s", "v", "v23.0").is_err());
        assert!(text_payload(Some(&"😀".repeat(4096)), "1", "m").is_ok());
        assert!(text_payload(None, "1", "m").is_err());
        let (payload, body, emojis) =
            reaction_prompt_payload("Run?", "1", "m", &["Approve".into(), "Deny".into()]).unwrap();
        assert_eq!(body, "Run?\n\nReact with:\n👍 Approve\n👎 Deny");
        assert_eq!(payload["text"]["body"], body);
        assert_eq!(emojis, ["👍", "👎"]);
        assert_eq!(reaction_emojis(3)[2], "3️⃣");
        assert!(reaction_prompt_payload("Run?", "1", "m", &vec!["x".into(); 11]).is_err());
        assert!(same_emoji("👍🏽", "👍") && same_emoji("1⃣", "1️⃣") && !same_emoji("", ""));
        assert!(!same_emoji("👎", "👍"));
        let buttons = prompt_payload("Run?", "1", "m", "p", &[("y".into(), "Yes".into())]).unwrap();
        assert_eq!(buttons["interactive"]["type"], "button");
        assert_eq!(
            buttons["interactive"]["action"]["buttons"][0]["reply"]["id"],
            "ac:p:y"
        );
        let long = [("a".into(), "A label longer than twenty".into())];
        let list = prompt_payload("Run?", "1", "m", "p", &long).unwrap();
        assert_eq!(list["interactive"]["type"], "list");
        let row = &list["interactive"]["action"]["sections"][0]["rows"][0];
        assert_eq!(row["title"].as_str().unwrap().chars().count(), 24);
        assert_eq!(row["description"], "A label longer than twenty");
        assert_eq!(
            typing_payload("wamid.1"),
            json!({"messaging_product":"whatsapp","status":"read","message_id":"wamid.1","typing_indicator":{"type":"text"}})
        );
    }
    #[test]
    fn signature_is_verified() {
        let mut mac = Hmac::<Sha256>::new_from_slice(b"s").unwrap();
        mac.update(b"body");
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        assert!(verify_signature("s", b"body", &signature));
        assert!(!verify_signature("s", b"body ", &signature));
    }
}
