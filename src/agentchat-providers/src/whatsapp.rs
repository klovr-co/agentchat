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
    // A direct source (see `direct_source`) has no message to reply to.
    let id = match source.metadata.get("message_id") {
        None | Some(Value::Null) => "",
        Some(id) => id.as_str().ok_or(invalid)?,
    };
    Ok(Destination {
        recipient: recipient.into(),
        message_id: id.into(),
    })
}

/// A source for messages sent to `wa_id` without replying to one of theirs.
/// WhatsApp delivers them only within 24 hours of that person's last message.
pub fn direct_source(phone: &str, wa_id: &str) -> Result<Message, &'static str> {
    if !digits(wa_id) {
        return Err("Expected a WhatsApp ID (digits only)");
    }
    Ok(Message {
        id: format!("whatsapp:{phone}:direct:{wa_id}").into(),
        conversation_id: format!("whatsapp:{phone}:{wa_id}").into(),
        channel: "whatsapp".into(),
        sender: Sender {
            id: wa_id.into(),
            display_name: None,
        },
        text: String::new(),
        role: Role::User,
        thread_id: None,
        addressed: true,
        attachments: Vec::new(),
        metadata: [
            ("phone_number_id".into(), json!(phone)),
            ("message_id".into(), Value::Null),
            ("wa_id".into(), json!(wa_id)),
        ]
        .into_iter()
        .collect(),
    })
}

/// `payload` replying to message `id`, or a new message when `id` is empty.
fn in_reply(mut payload: Value, id: &str) -> Value {
    if id.is_empty() {
        if let Some(object) = payload.as_object_mut() {
            object.remove("context");
        }
    }
    payload
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
    Ok(in_reply(
        json!({"messaging_product":"whatsapp","recipient_type":"individual","to":recipient,"type":"text","text":{"body":content,"preview_url":false},"context":{"message_id":id}}),
        id,
    ))
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

/// Where an inbound WhatsApp message goes when the application has a handler for
/// it: a button or list reply that is not a prompt answer is an action (by its
/// ID), a Flow response a view (by its `flow_token`), a template quick-reply
/// button an action (by its payload), and text starting with `/name` a command.
/// Reactions and other message types (location, contacts, order, ...) are events
/// by type. Text and media without a handler stay ordinary messages.
pub fn route_message(event: &Value) -> Option<agentchat_core::Route> {
    use agentchat_core::Route;
    let id = |value: &Value| {
        value
            .as_str()
            .filter(|id| !id.is_empty() && agentchat_core::answer_metadata(id).is_none())
            .map(str::to_owned)
    };
    match event["type"].as_str()? {
        "interactive" => {
            let interactive = &event["interactive"];
            match interactive["type"].as_str() {
                Some("nfm_reply") => {
                    let response: Value = interactive["nfm_reply"]["response_json"]
                        .as_str()
                        .and_then(|raw| serde_json::from_str(raw).ok())
                        .unwrap_or_default();
                    Some(match response["flow_token"].as_str() {
                        Some(token) if !token.is_empty() => Route::View {
                            callback_id: token.into(),
                        },
                        _ => Route::Other,
                    })
                }
                _ => {
                    let reply = if interactive["button_reply"].is_object() {
                        &interactive["button_reply"]["id"]
                    } else {
                        &interactive["list_reply"]["id"]
                    };
                    id(reply).map(|id| Route::Action {
                        action_ids: vec![id],
                    })
                }
            }
        }
        "button" => id(&event["button"]["payload"]).map(|id| Route::Action {
            action_ids: vec![id],
        }),
        "text" => agentchat_core::command_of(event["text"]["body"].as_str()?)
            .map(|command| Route::Command { command }),
        "image" | "audio" | "video" | "document" | "sticker" => None,
        kind => Some(Route::Event {
            event_type: kind.into(),
        }),
    }
}

/// One webhook item for the application: its route, the change `value` holding
/// only that message or status, and the message ID it would otherwise become.
#[derive(Debug, Clone, PartialEq)]
pub struct AppItem {
    pub route: agentchat_core::Route,
    pub payload: Value,
    pub message_id: Option<String>,
    /// The message to reply to with the handler's answer; None for statuses.
    pub source: Option<Message>,
}

/// The webhook's messages and statuses routed for the application; statuses are
/// `status` events.
pub fn app_items(payload: &Value, phone: &str) -> Vec<AppItem> {
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
            let only = |key: &str, item: &Value| {
                let mut single = value.clone();
                if let Some(object) = single.as_object_mut() {
                    object.remove("messages");
                    object.remove("statuses");
                    object.insert(key.into(), json!([item]));
                }
                single
            };
            for event in items(&value["messages"]) {
                if let Some(route) = route_message(event) {
                    let source = event["from"]
                        .as_str()
                        .zip(event["id"].as_str().filter(|id| !id.is_empty()))
                        .and_then(|(sender, id)| {
                            let mut source = direct_source(phone, sender).ok()?;
                            source.id = format!("whatsapp:{phone}:{id}").into();
                            source.metadata.insert("message_id".into(), json!(id));
                            Some(source)
                        });
                    found.push(AppItem {
                        route,
                        payload: only("messages", event),
                        message_id: event["id"]
                            .as_str()
                            .map(|id| format!("whatsapp:{phone}:{id}")),
                        source,
                    });
                }
            }
            for status in items(&value["statuses"]) {
                found.push(AppItem {
                    route: agentchat_core::Route::Event {
                        event_type: "status".into(),
                    },
                    payload: only("statuses", status),
                    message_id: None,
                    source: None,
                });
            }
        }
    }
    found
}

/// A rich reply as one Cloud API `interactive` object: `blocks` holds exactly one
/// (buttons, a list, a CTA URL or a Flow). `text` is kept as the message's text.
pub fn interactive_object(content: &agentchat_core::RichReply) -> Result<Value, &'static str> {
    if !content.attachments.is_empty() {
        return Err("WhatsApp rich replies do not support attachments");
    }
    match content.blocks.as_slice() {
        [block] if block.get("type").and_then(Value::as_str).is_some() => {
            Ok(Value::Object(block.clone()))
        }
        _ => Err("WhatsApp rich replies need exactly one interactive object in blocks"),
    }
}

/// An `interactive` message replying to the source message.
pub fn interactive_payload(recipient: &str, id: &str, interactive: Value) -> Value {
    in_reply(
        json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": recipient,
            "type": "interactive",
            "interactive": interactive,
            "context": {"message_id": id},
        }),
        id,
    )
}

/// Whether a notice for `user_id` can be an ordinary reply: WhatsApp chats are
/// one-to-one, so only for the person the conversation is with.
pub fn private_reply_allowed(source: &Message, user_id: &str) -> bool {
    source.sender.id == user_id
}

#[cfg(test)]
mod app_tests {
    use super::*;
    use agentchat_core::Route;

    #[test]
    fn messages_and_statuses_route_for_the_application() {
        let flow = json!({"id": "w1", "type": "interactive", "interactive": {"type": "nfm_reply",
            "nfm_reply": {"name": "flow", "body": "Sent", "response_json": "{\"flow_token\": \"signup\"}"}}});
        assert_eq!(
            route_message(&flow),
            Some(Route::View {
                callback_id: "signup".into()
            })
        );
        let press = json!({"type": "interactive", "interactive": {"type": "button_reply",
            "button_reply": {"id": "retry", "title": "Retry"}}});
        assert_eq!(
            route_message(&press),
            Some(Route::Action {
                action_ids: vec!["retry".into()]
            })
        );
        let token = agentchat_core::prompt_token("0123456789abcdef", "yes").unwrap();
        let answer = json!({"type": "interactive", "interactive": {"type": "button_reply",
            "button_reply": {"id": token, "title": "Yes"}}});
        assert_eq!(route_message(&answer), None);
        assert_eq!(
            route_message(&json!({"type": "text", "text": {"body": "/start"}})),
            Some(Route::Command {
                command: "/start".into()
            })
        );
        assert_eq!(
            route_message(&json!({"type": "text", "text": {"body": "hi"}})),
            None
        );
        assert_eq!(
            route_message(&json!({"type": "location"})),
            Some(Route::Event {
                event_type: "location".into()
            })
        );
        let payload = json!({"object": "whatsapp_business_account", "entry": [{"changes": [{
            "field": "messages", "value": {"messaging_product": "whatsapp",
            "metadata": {"phone_number_id": "1"}, "messages": [flow],
            "statuses": [{"id": "w0", "status": "read"}]}}]}]});
        let found = app_items(&payload, "1");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].message_id.as_deref(), Some("whatsapp:1:w1"));
        assert_eq!(found[0].payload["messages"][0]["id"], "w1");
        assert!(found[0].payload.get("statuses").is_none());
        assert_eq!(
            found[1].route,
            Route::Event {
                event_type: "status".into()
            }
        );
        assert!(app_items(&payload, "2").is_empty());
        let reply = agentchat_core::RichReply {
            text: "Sign up".into(),
            blocks: vec![json!({"type": "flow"}).as_object().unwrap().clone()],
            attachments: vec![],
        };
        assert_eq!(interactive_object(&reply).unwrap()["type"], "flow");
        let empty = agentchat_core::RichReply {
            blocks: vec![],
            ..reply
        };
        assert!(interactive_object(&empty).is_err());
    }
}
