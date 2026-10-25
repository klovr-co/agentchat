//! YCloud WhatsApp API normalization, routing, and authentication policy.
use crate::whatsapp::constant_time_eq;
use agentchat_core::{Attachment, Message, Role, Sender};
use hmac::{Hmac, Mac};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// Accepted clock skew between the `YCloud-Signature` timestamp and the receiver.
pub const SIGNATURE_TOLERANCE_SECONDS: i64 = 300;
/// Default ceiling for one media download.
pub const DEFAULT_MAX_MEDIA_BYTES: usize = 25 * 1024 * 1024;
/// `list_templates` stops after this many pages (YCloud accepts `page` 1–100).
pub const MAX_TEMPLATE_PAGES: u32 = 100;
pub const TEMPLATE_PAGE_SIZE: u32 = 100;

/// Normalized failure kinds an application can branch on (`YCloudError.kind`).
pub mod kind {
    /// Meta 131047: the 24-hour customer-service window has closed; offer a template.
    pub const WINDOW_EXPIRED: &str = "window_expired";
    /// Meta 132000: the number of template parameters does not match.
    pub const TEMPLATE_PARAMETERS: &str = "template_parameters";
    /// Meta 132001: the template does not exist in that language or is not approved.
    pub const TEMPLATE_NOT_FOUND: &str = "template_not_found";
    /// YCloud `WHATSAPP_TEMPLATE_UNAVAILABLE` (403): missing or unapproved template.
    pub const TEMPLATE_UNAVAILABLE: &str = "template_unavailable";
    /// YCloud `BALANCE_INSUFFICIENT` (403): top up before sending again.
    pub const BALANCE_INSUFFICIENT: &str = "balance_insufficient";
    /// HTTP 401 / YCloud `UNAUTHORIZED`: check the API key.
    pub const UNAUTHORIZED: &str = "unauthorized";
    /// HTTP 429: respect `Retry-After`; this adapter never retries.
    pub const RATE_LIMITED: &str = "rate_limited";
    /// Any other rejected request.
    pub const REJECTED: &str = "rejected";
    /// Timeout, connection failure, or unreadable response: the send may have happened.
    pub const UNCERTAIN: &str = "uncertain";
    pub const MEDIA_TOO_LARGE: &str = "media_too_large";
    pub const MEDIA_INTEGRITY: &str = "media_integrity";
    pub const MEDIA_UNAVAILABLE: &str = "media_unavailable";
}

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
fn text_of(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Message content shared by inbound messages and Business App echoes.
struct Content {
    text: String,
    attachments: Vec<Attachment>,
    media: Option<Value>,
}
fn content(event: &Value, kind: &str) -> Option<Content> {
    let mut attachments = Vec::new();
    let mut media_ref = None;
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
                let mut kept = Map::new();
                for key in ["id", "link", "sha256", "mime_type", "filename"] {
                    if let Some(value) = media[key].as_str() {
                        kept.insert(key.into(), json!(value));
                    }
                }
                media_ref = Some(Value::Object(kept));
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
    let text = text.as_str()?;
    if text.trim().is_empty() && attachments.is_empty() {
        return None;
    }
    Some(Content {
        text: text.into(),
        attachments,
        media: media_ref,
    })
}
fn metadata(
    phone: &str,
    id: &str,
    kind: &str,
    event: &Value,
    media: Option<Value>,
) -> Map<String, Value> {
    let mut metadata: Map<String, Value> = [
        ("phone_number".into(), json!(phone)),
        ("message_id".into(), json!(id)),
        ("message_type".into(), json!(kind)),
    ]
    .into_iter()
    .collect();
    for (key, field) in [("ycloud_message_id", "id"), ("waba_id", "wabaId")] {
        if let Some(value) = event[field].as_str() {
            metadata.insert(key.into(), json!(value));
        }
    }
    if let Some(media) = media {
        metadata.insert("media".into(), media);
    }
    metadata
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
    let Some(content) = content(event, kind) else {
        return Vec::new();
    };
    let mut metadata = metadata(phone, id, kind, event, content.media);
    metadata.insert("from".into(), json!(sender));
    if let Some(answer) = crate::whatsapp::reply_answer(event) {
        metadata.insert(agentchat_core::ANSWER_METADATA.into(), answer);
    }
    vec![Message {
        id: format!("ycloud:{phone}:{id}").into(),
        conversation_id: format!("ycloud:{phone}:{sender}").into(),
        channel: "ycloud".into(),
        sender: Sender {
            id: sender.into(),
            display_name: event["customerProfile"]["name"].as_str().map(str::to_owned),
        },
        text: content.text,
        role: Role::User,
        thread_id: None,
        addressed: true,
        attachments: content.attachments,
        metadata,
    }]
}

/// One `whatsapp.message.updated` delivery update, exactly as YCloud reported it.
/// Updates are not ordered: applications apply them forward only.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct StatusUpdate {
    pub event_id: Option<String>,
    pub status: String,
    pub timestamp: Option<String>,
    pub message_id: Option<String>,
    pub wamid: Option<String>,
    pub external_id: Option<String>,
    pub recipient: Option<String>,
    pub conversation_id: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub error_kind: Option<&'static str>,
    pub whatsapp_api_error: Option<WhatsAppApiError>,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct WhatsAppApiError {
    pub code: Option<String>,
    pub message: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub error_data: Value,
}
fn whatsapp_api_error(value: &Value) -> Option<WhatsAppApiError> {
    value.as_object()?;
    Some(WhatsAppApiError {
        code: text_of(&value["code"]),
        message: text_of(&value["message"]),
        kind: text_of(&value["type"]),
        error_data: value.get("error_data").cloned().unwrap_or(Value::Null),
    })
}

/// A verified webhook event, routed by `type`.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Customer messages for the agent handler.
    Messages(Vec<Message>),
    /// Delivery status for the event callback; never an agent turn.
    Status(StatusUpdate),
    /// A WhatsApp Business app reply (role `assistant`) for the event callback.
    Echo(Message),
    Ignored,
}
pub fn classify(payload: &Value, phone: &str, smb_echoes: bool) -> Event {
    match payload["type"].as_str() {
        Some("whatsapp.inbound_message.received") => {
            let messages = normalize(payload, phone);
            if messages.is_empty() {
                Event::Ignored
            } else {
                Event::Messages(messages)
            }
        }
        Some("whatsapp.message.updated") => {
            status_update(payload, phone).map_or(Event::Ignored, Event::Status)
        }
        Some("whatsapp.smb.message.echoes") if smb_echoes => {
            echo(payload, phone).map_or(Event::Ignored, Event::Echo)
        }
        _ => Event::Ignored,
    }
}
pub fn status_update(payload: &Value, phone: &str) -> Option<StatusUpdate> {
    if payload["type"] != "whatsapp.message.updated" {
        return None;
    }
    let message = &payload["whatsappMessage"];
    // Examples omit `from`; when present it must be this adapter's number.
    if message.get("from").is_some_and(|v| v != phone) {
        return None;
    }
    let status = message["status"].as_str().filter(|s| !s.is_empty())?;
    let message_id = text_of(&message["id"]);
    let wamid = text_of(&message["wamid"]);
    if message_id.is_none() && wamid.is_none() {
        return None;
    }
    let status_time = match status {
        "sent" => "sendTime",
        "delivered" => "deliverTime",
        "read" => "readTime",
        _ => "updateTime",
    };
    let timestamp = [
        &message[status_time],
        &message["updateTime"],
        &payload["createTime"],
    ]
    .into_iter()
    .find_map(text_of);
    let recipient = message["to"]
        .as_str()
        .filter(|v| e164(v))
        .map(str::to_owned);
    let whatsapp_api_error = whatsapp_api_error(&message["whatsappApiError"]);
    let error_code = text_of(&message["errorCode"]);
    let error_kind = (status == "failed").then(|| {
        error_kind(
            error_code.as_deref(),
            whatsapp_api_error
                .as_ref()
                .and_then(|e| e.code.as_deref())
                .or(error_code.as_deref()),
            None,
        )
    });
    Some(StatusUpdate {
        event_id: text_of(&payload["id"]),
        status: status.into(),
        timestamp,
        message_id,
        wamid,
        external_id: text_of(&message["externalId"]),
        conversation_id: recipient.as_ref().map(|to| format!("ycloud:{phone}:{to}")),
        recipient,
        error_code,
        error_message: text_of(&message["errorMessage"]),
        error_kind,
        whatsapp_api_error,
    })
}
/// `whatsapp.smb.message.echoes`: a reply staff sent from the WhatsApp Business app.
pub fn echo(payload: &Value, phone: &str) -> Option<Message> {
    if payload["type"] != "whatsapp.smb.message.echoes" {
        return None;
    }
    let event = &payload["whatsappMessage"];
    if event["from"] != phone {
        return None;
    }
    let customer = event["to"].as_str().filter(|v| e164(v))?;
    let id = event["wamid"].as_str().filter(|v| !v.is_empty())?;
    let kind = event["type"].as_str()?;
    let content = content(event, kind)?;
    let mut metadata = metadata(phone, id, kind, event, content.media);
    metadata.insert("to".into(), json!(customer));
    metadata.insert("echo".into(), json!(true));
    Some(Message {
        id: format!("ycloud:{phone}:{id}").into(),
        conversation_id: format!("ycloud:{phone}:{customer}").into(),
        channel: "ycloud".into(),
        sender: Sender {
            id: phone.into(),
            display_name: None,
        },
        text: content.text,
        role: Role::Assistant,
        thread_id: None,
        addressed: true,
        attachments: content.attachments,
        metadata,
    })
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
fn with_external_id(mut payload: Value, external_id: Option<&str>) -> Result<Value, &'static str> {
    if let Some(external_id) = external_id {
        if external_id.trim().is_empty() {
            return Err("YCloud external_id must be nonempty when given");
        }
        payload["externalId"] = json!(external_id);
    }
    Ok(payload)
}

pub fn text_payload(
    content: Option<&str>,
    sender: &str,
    recipient: &str,
    id: &str,
    external_id: Option<&str>,
) -> Result<Value, &'static str> {
    let content = content
        .filter(|s| !s.trim().is_empty())
        .ok_or("YCloud replies require nonempty text")?;
    if content.chars().count() > 4096 {
        return Err("YCloud replies must fit 4,096 characters; send separate replies");
    }
    with_external_id(
        json!({"from":sender,"to":recipient,"type":"text","text":{"body":content,"preview_url":false},"context":{"message_id":id}}),
        external_id,
    )
}
/// A prompt as an interactive reply to the source message, sent directly.
pub fn prompt_payload(
    sender: &str,
    recipient: &str,
    id: &str,
    text: &str,
    prompt: &str,
    choices: &[(String, String)],
) -> Result<Value, &'static str> {
    Ok(json!({
        "from": sender,
        "to": recipient,
        "type": "interactive",
        "interactive": crate::whatsapp::prompt_interactive(text, prompt, choices)?,
        "context": {"message_id": id},
    }))
}
pub fn template_payload(
    sender: &str,
    recipient: &str,
    name: &str,
    language: &str,
    components: &Value,
    external_id: Option<&str>,
) -> Result<Value, &'static str> {
    if !e164(recipient) {
        return Err("YCloud template recipient must be an E.164 number, e.g. +15551234567");
    }
    if name.is_empty()
        || !name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
    {
        return Err("YCloud template name must be lowercase letters, digits, and underscores");
    }
    if language.trim().is_empty() {
        return Err("YCloud template language_code required, e.g. en_US");
    }
    let mut template = json!({"name":name,"language":{"code":language}});
    match components {
        Value::Null => {}
        Value::Array(items) if items.is_empty() => {}
        Value::Array(_) => template["components"] = components.clone(),
        _ => return Err("YCloud template components must be a list"),
    }
    with_external_id(
        json!({"from":sender,"to":recipient,"type":"template","template":template}),
        external_id,
    )
}
fn receipt_ids(result: &Value) -> Result<(&str, Option<&str>), &'static str> {
    let ycloud_id = result["id"].as_str().filter(|s| !s.is_empty());
    let id = result["wamid"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or(ycloud_id)
        .ok_or("YCloud did not return a message receipt")?;
    Ok((id, ycloud_id))
}
fn set_receipt_ids(
    sent: &mut Message,
    id: &str,
    ycloud_id: Option<&str>,
    external_id: Option<&str>,
) {
    sent.metadata.insert("message_id".into(), json!(id));
    for (key, value) in [
        ("ycloud_message_id", ycloud_id),
        ("external_id", external_id),
    ] {
        match value {
            Some(value) => sent.metadata.insert(key.into(), json!(value)),
            None => sent.metadata.remove(key),
        };
    }
}
pub fn receipt(
    source: &Message,
    phone: &str,
    content: &str,
    result: &Value,
    external_id: Option<&str>,
) -> Result<Message, &'static str> {
    let (id, ycloud_id) = receipt_ids(result)?;
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
    sent.metadata.remove("media");
    set_receipt_ids(&mut sent, id, ycloud_id, external_id);
    Ok(sent)
}
/// Receipt for a send that has no source message (templates).
pub fn template_receipt(
    phone: &str,
    recipient: &str,
    name: &str,
    language: &str,
    result: &Value,
    external_id: Option<&str>,
) -> Result<Message, &'static str> {
    let (id, ycloud_id) = receipt_ids(result)?;
    let mut sent = Message {
        id: format!("ycloud:{phone}:{id}").into(),
        conversation_id: format!("ycloud:{phone}:{recipient}").into(),
        channel: "ycloud".into(),
        sender: Sender {
            id: phone.into(),
            display_name: None,
        },
        text: String::new(),
        role: Role::Assistant,
        thread_id: None,
        addressed: true,
        attachments: Vec::new(),
        metadata: [
            ("phone_number".into(), json!(phone)),
            ("to".into(), json!(recipient)),
            ("message_type".into(), json!("template")),
            ("template_name".into(), json!(name)),
            ("template_language".into(), json!(language)),
        ]
        .into_iter()
        .collect(),
    };
    set_receipt_ids(&mut sent, id, ycloud_id, external_id);
    Ok(sent)
}

/// A rejected YCloud request, classified without its message text or credentials.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub kind: &'static str,
    pub status: Option<i64>,
    pub code: Option<String>,
    pub whatsapp_code: Option<String>,
    pub request_id: Option<String>,
}
impl ApiError {
    pub fn description(&self) -> String {
        let mut text = format!("YCloud request failed ({})", self.kind);
        if let Some(status) = self.status {
            text.push_str(&format!("; HTTP {status}"));
        }
        if let Some(code) = &self.code {
            text.push_str(&format!("; code {code}"));
        }
        if let Some(code) = &self.whatsapp_code {
            text.push_str(&format!("; WhatsApp code {code}"));
        }
        text
    }
}
pub fn error_kind(
    ycloud_code: Option<&str>,
    whatsapp_code: Option<&str>,
    status: Option<i64>,
) -> &'static str {
    match (whatsapp_code, ycloud_code) {
        (Some("131047"), _) | (_, Some("131047")) => kind::WINDOW_EXPIRED,
        (Some("132000"), _) | (_, Some("132000")) => kind::TEMPLATE_PARAMETERS,
        (Some("132001"), _) | (_, Some("132001")) => kind::TEMPLATE_NOT_FOUND,
        (_, Some("WHATSAPP_TEMPLATE_UNAVAILABLE")) => kind::TEMPLATE_UNAVAILABLE,
        (_, Some("BALANCE_INSUFFICIENT")) => kind::BALANCE_INSUFFICIENT,
        (_, Some("UNAUTHORIZED")) => kind::UNAUTHORIZED,
        _ => match status {
            Some(401) => kind::UNAUTHORIZED,
            Some(429) => kind::RATE_LIMITED,
            _ => kind::REJECTED,
        },
    }
}
/// Classifies a non-2xx response body (`{"error":{"status","code","whatsappApiError",…}}`).
pub fn api_error(status: i64, body: &Value) -> ApiError {
    let error = &body["error"];
    let code = text_of(&error["code"]);
    let whatsapp_code = text_of(&error["whatsappApiError"]["code"]);
    ApiError {
        kind: error_kind(code.as_deref(), whatsapp_code.as_deref(), Some(status)),
        status: Some(status),
        code,
        whatsapp_code,
        request_id: text_of(&error["requestId"]),
    }
}

/// A WhatsApp template as returned by `GET /whatsapp/templates`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Template {
    pub name: String,
    pub language: String,
    pub category: Option<String>,
    pub status: Option<String>,
    pub body: Option<String>,
    pub components: Value,
}
/// Parses one page; the bool says whether another page may follow.
pub fn template_page(page: &Value, limit: u32) -> Result<(Vec<Template>, bool), &'static str> {
    let items = page["items"]
        .as_array()
        .ok_or("YCloud returned an unreadable template page")?;
    let templates = items
        .iter()
        .filter_map(|item| {
            let components = item.get("components").cloned().unwrap_or(json!([]));
            let body = components.as_array().and_then(|list| {
                list.iter()
                    .find(|c| c["type"] == "BODY")
                    .and_then(|c| c["text"].as_str())
                    .map(str::to_owned)
            });
            Some(Template {
                name: item["name"].as_str()?.into(),
                language: item["language"].as_str()?.into(),
                category: text_of(&item["category"]),
                status: text_of(&item["status"]),
                body,
                components,
            })
        })
        .collect();
    let length = page["length"].as_u64().unwrap_or(items.len() as u64);
    Ok((templates, length >= u64::from(limit) && !items.is_empty()))
}

/// Where and how to fetch one media object.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaRef {
    pub link: String,
    pub id: Option<String>,
    pub sha256: Option<String>,
    pub mime_type: Option<String>,
    pub filename: Option<String>,
}
impl MediaRef {
    pub fn name(&self) -> String {
        self.filename
            .clone()
            .or_else(|| self.id.clone())
            .unwrap_or_else(|| "ycloud-media".into())
    }
}
/// Accepts a message's `metadata["media"]` mapping (or the mapping itself).
pub fn media_ref(value: &Value) -> Result<MediaRef, &'static str> {
    let media = if value.get("link").is_some() {
        value
    } else {
        &value["media"]
    };
    let link = media["link"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or("This message has no YCloud media link")?;
    let field = |key: &str| {
        media[key]
            .as_str()
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    Ok(MediaRef {
        link: link.into(),
        id: field("id"),
        sha256: field("sha256"),
        mime_type: field("mime_type"),
        filename: field("filename"),
    })
}
/// Only `{api_root}/whatsapp/media/…` on the API origin is fetched (SSRF guard).
pub fn check_media_link(link: &str, api_root: &str) -> Result<(), &'static str> {
    let rejected = "Refusing to download media from outside the YCloud API";
    let (Ok(link), Ok(root)) = (url::Url::parse(link), url::Url::parse(api_root)) else {
        return Err(rejected);
    };
    let prefix = format!("{}/whatsapp/media/", root.path().trim_end_matches('/'));
    if link.scheme() != root.scheme()
        || link.host_str().is_none()
        || link.host_str() != root.host_str()
        || link.port_or_known_default() != root.port_or_known_default()
        || !link.username().is_empty()
        || link.password().is_some()
        || !link.path().starts_with(&prefix)
    {
        return Err(rejected);
    }
    Ok(())
}
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(TABLE[(n >> (18 - 6 * i)) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }
    out
}
/// YCloud sends base64 digests (`LeRpQJq…=`); hex is accepted as well.
pub fn media_digest_matches(content: &[u8], expected: &str) -> bool {
    let digest = Sha256::digest(content);
    let expected = expected.trim();
    let hex = hex::encode(digest);
    let b64 = base64(&digest);
    constant_time_eq(hex.as_bytes(), expected.to_ascii_lowercase().as_bytes())
        || constant_time_eq(b64.as_bytes(), expected.as_bytes())
        || constant_time_eq(b64.trim_end_matches('=').as_bytes(), expected.as_bytes())
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
    const PHONE: &str = "+15550001111";
    fn sign(secret: &str, t: i64, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(format!("{t}.").as_bytes());
        mac.update(body);
        format!("t={t},s={}", hex::encode(mac.finalize().into_bytes()))
    }
    #[test]
    fn validation_and_payload() {
        assert!(validate_config(PHONE, "k", "s").is_ok());
        assert!(validate_config("15550001111", "k", "s").is_err());
        assert!(validate_config(PHONE, " ", "s").is_err());
        assert!(text_payload(Some(&"😀".repeat(4096)), "+1", "+2", "m", None).is_ok());
        assert!(text_payload(Some(&"a".repeat(4097)), "+1", "+2", "m", None).is_err());
        assert!(text_payload(None, "+1", "+2", "m", None).is_err());
        let payload = text_payload(Some("hi"), "+1", "+2", "m", Some("row-7")).unwrap();
        assert_eq!(payload["externalId"], "row-7");
        assert!(text_payload(Some("hi"), "+1", "+2", "m", Some(" ")).is_err());
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
            let mut message = json!({"wamid":"wamid.1","from":"+15551234567","to":PHONE,"type":"text","text":{"body":"hi"}});
            message
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            json!({"type":"whatsapp.inbound_message.received","whatsappInboundMessage":message})
        };
        assert_eq!(normalize(&event(json!({})), PHONE).len(), 1);
        assert!(normalize(&event(json!({})), "+15559999999").is_empty());
        assert!(normalize(&event(json!({"groupId":"g"})), PHONE).is_empty());
    }
    #[test]
    fn status_updates_keep_raw_fields_and_classify_failures() {
        let failed = json!({"id":"evt_1","type":"whatsapp.message.updated","createTime":"2026-10-21T00:00:00Z",
            "whatsappMessage":{"id":"yc1","wamid":"wamid.1","to":"+15551234567","status":"failed",
            "externalId":"row-7","errorCode":131047,"errorMessage":"Re-engagement message",
            "whatsappApiError":{"code":"131047","message":"m","type":"OAuthException","error_data":{"details":"d"}}}});
        let Event::Status(update) = classify(&failed, PHONE, false) else {
            panic!("status event")
        };
        assert_eq!(update.status, "failed");
        assert_eq!(update.error_code.as_deref(), Some("131047"));
        assert_eq!(update.error_kind, Some(kind::WINDOW_EXPIRED));
        assert_eq!(update.timestamp.as_deref(), Some("2026-10-21T00:00:00Z"));
        assert_eq!(update.external_id.as_deref(), Some("row-7"));
        assert_eq!(
            update.conversation_id.as_deref(),
            Some("ycloud:+15550001111:+15551234567")
        );
        let read = json!({"type":"whatsapp.message.updated","whatsappMessage":{"wamid":"w","status":"read","readTime":"t-read","deliverTime":"t-del"}});
        let Event::Status(update) = classify(&read, PHONE, false) else {
            panic!("status event")
        };
        assert_eq!(update.timestamp.as_deref(), Some("t-read"));
        assert_eq!(update.error_kind, None);
        let foreign = json!({"type":"whatsapp.message.updated","whatsappMessage":{"wamid":"w","from":"+1999","status":"sent"}});
        assert_eq!(classify(&foreign, PHONE, false), Event::Ignored);
    }
    #[test]
    fn echoes_are_opt_in_assistant_messages_in_the_customer_conversation() {
        let payload = json!({"id":"evt","type":"whatsapp.smb.message.echoes","whatsappMessage":{
            "id":"yc","wamid":"wamid.e","from":PHONE,"to":"+15551234567","type":"text","text":{"body":"On it"}}});
        assert_eq!(classify(&payload, PHONE, false), Event::Ignored);
        let Event::Echo(message) = classify(&payload, PHONE, true) else {
            panic!("echo")
        };
        assert_eq!(message.role, Role::Assistant);
        assert_eq!(
            message.conversation_id.to_string(),
            "ycloud:+15550001111:+15551234567"
        );
        assert!(destination(&message, PHONE).is_err());
    }
    #[test]
    fn errors_map_to_kinds() {
        let body =
            json!({"error":{"status":400,"code":"BAD_REQUEST","whatsappApiError":{"code":131047}}});
        assert_eq!(api_error(400, &body).kind, kind::WINDOW_EXPIRED);
        let body = json!({"error":{"code":"BALANCE_INSUFFICIENT"}});
        assert_eq!(api_error(403, &body).kind, kind::BALANCE_INSUFFICIENT);
        let body = json!({"error":{"code":"WHATSAPP_TEMPLATE_UNAVAILABLE"}});
        assert_eq!(api_error(403, &body).kind, kind::TEMPLATE_UNAVAILABLE);
        let body = json!({"error":{"whatsappApiError":{"code":"132000"}}});
        assert_eq!(api_error(400, &body).kind, kind::TEMPLATE_PARAMETERS);
        assert_eq!(
            api_error(
                400,
                &json!({"error":{"whatsappApiError":{"code":"132001"}}})
            )
            .kind,
            kind::TEMPLATE_NOT_FOUND
        );
        assert_eq!(api_error(429, &json!(null)).kind, kind::RATE_LIMITED);
        assert_eq!(api_error(500, &json!("x")).kind, kind::REJECTED);
    }
    #[test]
    fn media_links_digest_and_templates() {
        let root = "https://api.ycloud.com/v2";
        assert!(check_media_link(
            "https://api.ycloud.com/v2/whatsapp/media/download/1?sig=x",
            root
        )
        .is_ok());
        for link in [
            "http://api.ycloud.com/v2/whatsapp/media/download/1",
            "https://api.ycloud.com.evil.test/v2/whatsapp/media/download/1",
            "https://evil.test/v2/whatsapp/media/download/1",
            "https://user@api.ycloud.com/v2/whatsapp/media/download/1",
            "https://api.ycloud.com:8443/v2/whatsapp/media/download/1",
            "https://api.ycloud.com/v2/balance",
            "file:///etc/passwd",
        ] {
            assert!(check_media_link(link, root).is_err(), "{link}");
        }
        // sha256("abc") in both encodings YCloud may use.
        assert!(media_digest_matches(
            b"abc",
            "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0="
        ));
        assert!(media_digest_matches(
            b"abc",
            "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"
        ));
        assert!(!media_digest_matches(
            b"abd",
            "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0="
        ));
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"a"), "YQ==");
        assert!(template_payload(PHONE, "+1555", "order_ready", "en", &json!(null), None).is_ok());
        assert!(template_payload(PHONE, "1555", "order_ready", "en", &json!([]), None).is_err());
        assert!(template_payload(PHONE, "+1555", "Order Ready", "en", &json!([]), None).is_err());
        let page = json!({"length":1,"items":[{"name":"t","language":"en","status":"APPROVED","category":"UTILITY",
            "components":[{"type":"BODY","text":"Hi {{1}}"}]}]});
        let (templates, more) = template_page(&page, 100).unwrap();
        assert_eq!(templates[0].body.as_deref(), Some("Hi {{1}}"));
        assert!(!more);
    }
}
