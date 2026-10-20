//! YCloud WhatsApp API normalization, routing, and authentication policy.
use crate::whatsapp::constant_time_eq;
use agentchat_core::{Attachment, Message, Role, Sender};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

/// Accepted clock skew between the `YCloud-Signature` timestamp and the receiver.
pub const SIGNATURE_TOLERANCE_SECONDS: i64 = 300;

pub fn validate_config(phone: &str, api_key: &str, secret: &str) -> Result<(), &'static str> {
    if [phone, api_key, secret].iter().any(|v| v.trim().is_empty()) {
        return Err("YCloud phone_number, api_key, webhook_secret required");
    }
    if !e164(phone) {
        return Err("YCloud phone_number must be the E.164 business number, e.g. +15550001111");
    }
    Ok(())
}
fn digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|c| c.is_ascii_digit())
}
fn e164(value: &str) -> bool {
    value.strip_prefix('+').is_some_and(digits)
}
pub fn normalize(payload: &Value, phone: &str) -> Vec<Message> {
    let event = &payload["whatsappInboundMessage"];
    if payload["type"] != "whatsapp.inbound_message.received"
        || event["to"] != phone
        || event.get("groupId").is_some_and(|v| !v.is_null())
    {
        return Vec::new();
    }
    let (Some(sender), Some(id), Some(kind)) = (
        event["from"].as_str(),
        event["wamid"].as_str(),
        event["type"].as_str(),
    ) else {
        return Vec::new();
    };
    if !e164(sender) || id.is_empty() {
        return Vec::new();
    }
    let mut attachments = Vec::new();
    let empty = json!("");
    let text = match kind {
        "text" => event["text"].get("body").unwrap_or(&empty),
        "image" | "audio" | "video" | "document" | "sticker" => {
            let media = &event[kind];
            let reference = [&media["id"], &media["link"]]
                .into_iter()
                .find_map(|v| v.as_str().filter(|v| !v.is_empty()));
            if let Some(reference) = reference {
                attachments.push(Attachment {
                    id: reference.into(),
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
        return Vec::new();
    };
    if text.trim().is_empty() && attachments.is_empty() {
        return Vec::new();
    }
    let mut metadata: serde_json::Map<String, Value> = [
        ("phone_number".into(), json!(phone)),
        ("message_id".into(), json!(id)),
        ("from".into(), json!(sender)),
        ("message_type".into(), json!(kind)),
    ]
    .into_iter()
    .collect();
    for (key, field) in [("ycloud_message_id", "id"), ("waba_id", "wabaId")] {
        if let Some(value) = event[field].as_str() {
            metadata.insert(key.into(), json!(value));
        }
    }
    vec![Message {
        id: format!("ycloud:{phone}:{id}").into(),
        conversation_id: format!("ycloud:{phone}:{sender}").into(),
        channel: "ycloud".into(),
        sender: Sender {
            id: sender.into(),
            display_name: event["customerProfile"]["name"].as_str().map(str::to_owned),
        },
        text: text.into(),
        role: Role::User,
        thread_id: None,
        addressed: true,
        attachments,
        metadata: metadata.into_iter().collect(),
    }]
}
pub struct Destination {
    pub recipient: String,
    pub message_id: String,
}
pub fn destination(source: &Message, phone: &str) -> Result<Destination, &'static str> {
    let invalid = "Source does not belong to this YCloud adapter";
    if source.channel != "ycloud" || source.metadata.get("phone_number") != Some(&json!(phone)) {
        return Err(invalid);
    }
    let recipient = source
        .metadata
        .get("from")
        .and_then(Value::as_str)
        .filter(|value| e164(value))
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
    sender: &str,
    recipient: &str,
    id: &str,
) -> Result<Value, &'static str> {
    let content = content
        .filter(|s| !s.trim().is_empty())
        .ok_or("YCloud replies require nonempty text")?;
    if content.chars().count() > 4096 {
        return Err("YCloud replies must fit 4,096 characters; send separate replies");
    }
    Ok(
        json!({"from":sender,"to":recipient,"type":"text","text":{"body":content,"preview_url":false},"context":{"message_id":id}}),
    )
}
pub fn receipt(
    source: &Message,
    phone: &str,
    content: &str,
    result: &Value,
) -> Result<Message, &'static str> {
    let ycloud_id = result["id"].as_str().filter(|s| !s.is_empty());
    let id = result["wamid"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or(ycloud_id)
        .ok_or("YCloud did not return a message receipt")?;
    let mut sent = source.clone();
    sent.id = format!("ycloud:{phone}:{id}").into();
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
    match ycloud_id {
        Some(value) => sent
            .metadata
            .insert("ycloud_message_id".into(), json!(value)),
        None => sent.metadata.remove("ycloud_message_id"),
    };
    Ok(sent)
}
/// Verifies `YCloud-Signature: t={unix seconds},s={hex HMAC-SHA256("{t}.{raw body}")}`.
pub fn verify_signature(secret: &str, raw: &[u8], supplied: &str, now: i64) -> bool {
    let (mut timestamp, mut signature) = (None, None);
    for part in supplied.split(',') {
        match part.trim().split_once('=') {
            Some(("t", value)) => timestamp = Some(value),
            Some(("s", value)) => signature = Some(value),
            _ => {}
        }
    }
    let (Some(timestamp), Some(signature)) = (timestamp, signature) else {
        return false;
    };
    let Some(sent) = Some(timestamp)
        .filter(|v| digits(v))
        .and_then(|v| v.parse::<i64>().ok())
    else {
        return false;
    };
    if (now - sent).abs() > SIGNATURE_TOLERANCE_SECONDS {
        return false;
    }
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(raw);
    let expected = hex::encode(mac.finalize().into_bytes());
    constant_time_eq(
        expected.as_bytes(),
        signature.to_ascii_lowercase().as_bytes(),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn sign(secret: &str, t: i64, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(format!("{t}.").as_bytes());
        mac.update(body);
        format!("t={t},s={}", hex::encode(mac.finalize().into_bytes()))
    }
    #[test]
    fn validation_and_payload() {
        assert!(validate_config("+15550001111", "k", "s").is_ok());
        assert!(validate_config("15550001111", "k", "s").is_err());
        assert!(validate_config("+15550001111", " ", "s").is_err());
        assert!(text_payload(Some(&"😀".repeat(4096)), "+1", "+2", "m").is_ok());
        assert!(text_payload(Some(&"a".repeat(4097)), "+1", "+2", "m").is_err());
        assert!(text_payload(None, "+1", "+2", "m").is_err());
    }
    #[test]
    fn signature_is_verified_with_tolerance() {
        let header = sign("whsec_s", 1_000, b"body");
        assert!(verify_signature("whsec_s", b"body", &header, 1_000));
        assert!(verify_signature("whsec_s", b"body", &header, 1_300));
        assert!(!verify_signature("whsec_s", b"body", &header, 1_301));
        assert!(!verify_signature("whsec_s", b"body ", &header, 1_000));
        assert!(!verify_signature("other", b"body", &header, 1_000));
        assert!(!verify_signature("whsec_s", b"body", "s=abc", 1_000));
        assert!(!verify_signature("whsec_s", b"body", "", 1_000));
    }
    #[test]
    fn group_and_foreign_numbers_are_ignored() {
        let event = |extra: Value| {
            let mut message = json!({"wamid":"wamid.1","from":"+15551234567","to":"+15550001111","type":"text","text":{"body":"hi"}});
            message
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            json!({"type":"whatsapp.inbound_message.received","whatsappInboundMessage":message})
        };
        assert_eq!(normalize(&event(json!({})), "+15550001111").len(), 1);
        assert!(normalize(&event(json!({})), "+15559999999").is_empty());
        assert!(normalize(&event(json!({"groupId":"g"})), "+15550001111").is_empty());
    }
}
