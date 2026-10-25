//! Telegram Bot API policy, shared independently of Python transport objects.
use agentchat_core::{Attachment, Message, Role, Sender};
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use std::{collections::HashSet, sync::LazyLock};
pub const WEBHOOK_PATH: &str = "/webhooks/telegram";
fn items(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}
fn nonempty(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.is_empty())
}
fn sender(user: &Value) -> Option<Sender> {
    let id = user["id"].as_i64()?;
    if user["is_bot"] != false {
        return None;
    }
    let name = [user["first_name"].as_str(), user["last_name"].as_str()]
        .into_iter()
        .flatten()
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Some(Sender {
        id: id.to_string(),
        display_name: (!name.is_empty()).then_some(name),
    })
}
pub struct BotAddress {
    pub is_addressed: bool,
    pub text: String,
}
pub fn strip_bot_address(
    text: &str,
    entities: &[Value],
    bot_id: i64,
    username: &str,
) -> BotAddress {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mention = format!("@{}", username.to_lowercase());
    let mut addressed = false;
    let mut removals = Vec::new();
    for entity in entities {
        let (Some(start), Some(length)) = (entity["offset"].as_u64(), entity["length"].as_u64())
        else {
            continue;
        };
        let (start, length) = (start as usize, length as usize);
        let Some(end) = start.checked_add(length) else {
            continue;
        };
        if length == 0 || end > units.len() {
            continue;
        }
        let Ok(value) = String::from_utf16(&units[start..end]) else {
            continue;
        };
        match entity["type"].as_str() {
            Some("mention") if value.to_lowercase() == mention => {
                addressed = true;
                removals.push((start, end));
            }
            Some("bot_command") if value.to_lowercase().ends_with(&mention) => {
                addressed = true;
                if let Some(suffix_start) = end.checked_sub(mention.encode_utf16().count()) {
                    if suffix_start >= start
                        && String::from_utf16(&units[suffix_start..end]).is_ok()
                    {
                        removals.push((suffix_start, end));
                    }
                }
            }
            Some("text_mention") if entity["user"]["id"].as_i64() == Some(bot_id) => {
                addressed = true;
            }
            _ => {}
        }
    }
    removals.sort_unstable();
    let mut kept = Vec::new();
    let mut cursor = 0;
    for (start, end) in removals {
        if start < cursor {
            continue;
        }
        kept.extend_from_slice(&units[cursor..start]);
        cursor = end;
    }
    kept.extend_from_slice(&units[cursor..]);
    BotAddress {
        is_addressed: addressed,
        text: String::from_utf16_lossy(&kept).trim().into(),
    }
}
pub fn attachments(message: &Value) -> Vec<Attachment> {
    let mut found = Vec::new();
    if let Some(photo) = items(&message["photo"]).iter().rfind(|v| v.is_object()) {
        if let Some(id) = nonempty(&photo["file_id"]) {
            found.push(Attachment {
                id: id.into(),
                name: None,
                media_type: Some("image/jpeg".into()),
                size: photo["file_size"].as_u64(),
            });
        }
    }
    for kind in [
        "animation",
        "document",
        "audio",
        "video",
        "voice",
        "video_note",
        "sticker",
    ] {
        let media = &message[kind];
        let Some(id) = nonempty(&media["file_id"]) else {
            continue;
        };
        if found.iter().any(|v| v.id == id) {
            continue;
        }
        found.push(Attachment {
            id: id.into(),
            name: nonempty(&media["file_name"]).map(str::to_owned),
            media_type: nonempty(&media["mime_type"]).map(str::to_owned),
            size: media["file_size"].as_u64(),
        });
    }
    found
}
#[derive(Clone, Copy)]
struct SupportedChat<'a> {
    id: i64,
    kind: &'a str,
    message_id: i64,
}
fn supported_chat(message: &Value) -> Option<SupportedChat<'_>> {
    let chat = &message["chat"];
    let id = chat["id"].as_i64()?;
    let kind = chat["type"].as_str()?;
    let message_id = message["message_id"].as_i64()?;
    if !matches!(kind, "private" | "group" | "supergroup") || message_id <= 0 {
        return None;
    }
    Some(SupportedChat {
        id,
        kind,
        message_id,
    })
}
fn build(
    bot: i64,
    local: &str,
    chat: SupportedChat<'_>,
    message: &Value,
    sender: Sender,
    text: String,
    attachments: Vec<Attachment>,
) -> Message {
    let thread = if message["is_topic_message"] == true {
        message["message_thread_id"].as_i64()
    } else {
        None
    };
    let base = format!("telegram:{bot}:{}", chat.id);
    Message {
        id: format!("telegram:{bot}:{local}").into(),
        conversation_id: thread.map(|v| format!("{base}:{v}")).unwrap_or(base).into(),
        channel: "telegram".into(),
        sender,
        text,
        role: Role::User,
        thread_id: thread.map(|v| v.to_string()),
        addressed: true,
        attachments,
        metadata: [
            ("bot_id".into(), json!(bot)),
            ("chat_id".into(), json!(chat.id)),
            ("chat_type".into(), json!(chat.kind)),
            ("message_id".into(), json!(chat.message_id)),
            ("message_thread_id".into(), json!(thread)),
        ]
        .into_iter()
        .collect(),
    }
}
pub fn normalize(update: &Value, bot: i64, username: &str) -> Option<Message> {
    if let Some(callback) = update.get("callback_query") {
        let message = &callback["message"];
        let chat = supported_chat(message)?;
        let data = nonempty(&callback["data"])?;
        let id = nonempty(&callback["id"])?;
        let sender = sender(&callback["from"])?;
        if message["date"] == 0 {
            return None;
        }
        let text = items(&message["reply_markup"]["inline_keyboard"])
            .iter()
            .flat_map(items)
            .find(|b| b["callback_data"] == data && b["text"].is_string())
            .and_then(|b| b["text"].as_str())
            .unwrap_or(data);
        let mut result = build(
            bot,
            &format!("callback:{id}"),
            chat,
            message,
            sender,
            text.into(),
            vec![],
        );
        result
            .metadata
            .insert("callback_query_id".into(), json!(id));
        result.metadata.insert("callback_data".into(), json!(data));
        if let Some(answer) = agentchat_core::answer_metadata(data) {
            result
                .metadata
                .insert(agentchat_core::ANSWER_METADATA.into(), answer);
        }
        return Some(result);
    }
    let message = &update["message"];
    let sender = sender(&message["from"])?;
    let chat = supported_chat(message)?;
    let (text, entities) = if let Some(t) = message["text"].as_str() {
        (t, items(&message["entities"]))
    } else if let Some(t) = message["caption"].as_str() {
        (t, items(&message["caption_entities"]))
    } else {
        ("", &[][..])
    };
    let address = strip_bot_address(text, entities, bot, username);
    if chat.kind != "private"
        && !address.is_addressed
        && message["reply_to_message"]["from"]["id"].as_i64() != Some(bot)
    {
        return None;
    }
    let files = attachments(message);
    if address.text.is_empty() && files.is_empty() {
        return None;
    }
    Some(build(
        bot,
        &format!("{}:{}", chat.id, chat.message_id),
        chat,
        message,
        sender,
        address.text,
        files,
    ))
}
#[derive(Clone, Debug)]
pub struct Destination {
    pub chat_id: i64,
    pub message_id: i64,
    pub thread_id: Option<i64>,
}
pub fn destination(source: &Message, bot: i64) -> Result<Destination, &'static str> {
    let bad = "Source does not belong to this Telegram adapter";
    if source.channel != "telegram"
        || source.metadata.get("bot_id").and_then(Value::as_i64) != Some(bot)
    {
        return Err(bad);
    }
    let chat_id = source
        .metadata
        .get("chat_id")
        .and_then(Value::as_i64)
        .ok_or(bad)?;
    let message_id = source
        .metadata
        .get("message_id")
        .and_then(Value::as_i64)
        .ok_or(bad)?;
    let thread_id = match source.metadata.get("message_thread_id") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_i64().ok_or(bad)?),
    };
    Ok(Destination {
        chat_id,
        message_id,
        thread_id,
    })
}
impl Destination {
    pub fn route(&self, mut payload: Value, quote: bool) -> Value {
        payload["chat_id"] = json!(self.chat_id);
        if quote {
            payload["reply_parameters"] =
                json!({"message_id":self.message_id,"allow_sending_without_reply":true});
        }
        if let Some(id) = self.thread_id {
            payload["message_thread_id"] = json!(id);
        }
        payload
    }
}
pub fn receipt(
    source: &Message,
    bot: i64,
    name: Option<String>,
    text: &str,
    result: &Value,
) -> Result<Message, &'static str> {
    let id = result["message_id"]
        .as_i64()
        .filter(|i| *i > 0)
        .ok_or("Telegram did not return a message receipt")?;
    let dest = destination(source, bot)?;
    let mut sent = source.clone();
    sent.id = format!("telegram:{bot}:{}:{id}", dest.chat_id).into();
    sent.sender = Sender {
        id: bot.to_string(),
        display_name: name,
    };
    sent.text = text.into();
    sent.role = Role::Assistant;
    sent.addressed = true;
    sent.attachments.clear();
    sent.metadata.remove("callback_query_id");
    sent.metadata.remove("callback_data");
    sent.metadata.insert("message_id".into(), json!(id));
    Ok(sent)
}
pub fn validate_config(
    token: &str,
    url: Option<&str>,
    secret: Option<&str>,
) -> Result<i64, &'static str> {
    if token.trim().is_empty() {
        return Err("Telegram bot_token is required");
    }
    let bot = token
        .split_once(':')
        .filter(|(id, key)| {
            !id.is_empty()
                && id.bytes().all(|c| c.is_ascii_digit())
                && !key.is_empty()
                && key
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        })
        .and_then(|(id, _)| id.parse().ok())
        .ok_or("Telegram bot_token must look like 123456:ABC...")?;
    if url.is_some() != secret.is_some() {
        return Err("Telegram webhook_url and webhook_secret must be given together");
    }
    if let Some(url) = url {
        if !url::Url::parse(url).is_ok_and(|v| {
            v.scheme() == "https" && v.host_str().is_some() && v.path() == WEBHOOK_PATH
        }) {
            return Err("Telegram webhook_url must be https and end in /webhooks/telegram");
        }
    }
    if let Some(secret) = secret {
        if secret.is_empty()
            || secret.len() > 256
            || !secret
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            return Err("Telegram webhook_secret must be 1-256 of A-Z, a-z, 0-9, _ and -");
        }
    }
    Ok(bot)
}
pub fn retry_delay(attempt: u32, retry_after: Option<i64>, base: f64, max: f64) -> f64 {
    if let Some(delay) = retry_after.filter(|v| *v != 0) {
        delay as f64
    } else {
        (base * 2f64.powi(attempt.min(i32::MAX as u32) as i32)).min(max)
    }
}
pub fn plain_filename(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !matches!(name, "." | "..")
        && !name
            .chars()
            .any(|c| c == '/' || c == '\\' || (c as u32) < 32)
}
pub fn truncate_caption(text: &str) -> String {
    let mut size = 0;
    text.chars()
        .take_while(|c| {
            size += c.len_utf16();
            size <= 1024
        })
        .collect()
}
pub fn download_path(file: &Value, limit: usize) -> Result<String, String> {
    if file["file_size"].as_u64().is_some_and(|n| n > limit as u64) {
        return Err(download_limit_error(limit));
    }
    let path = nonempty(&file["file_path"])
        .filter(|s| !s.starts_with('/') && !s.split('/').any(|p| p == ".."))
        .ok_or("This Telegram file is unavailable. Send the attachment again.")?;
    Ok(path.into())
}
pub fn download_limit_error(limit: usize) -> String {
    format!(
        "Telegram file exceeds the {} MB download limit",
        limit as f64 / (1024. * 1024.)
    )
}
#[derive(Debug)]
pub struct ApiError {
    pub description: String,
    pub error_code: Option<i64>,
    pub retry_after: Option<i64>,
}
impl ApiError {
    pub fn is_transient(&self) -> bool {
        self.error_code.is_none_or(|c| c == 429 || c >= 500)
    }
}
pub fn unwrap_result(method: &str, status: i64, body: &Value) -> Result<Value, ApiError> {
    if body["ok"] == true {
        if let Some(result) = body.get("result") {
            return Ok(result.clone());
        }
    }
    if !body.is_object() {
        return Err(ApiError {
            description: format!("Telegram {method} failed (HTTP {status})"),
            error_code: Some(status),
            retry_after: None,
        });
    }
    Err(ApiError {
        description: format!(
            "Telegram {method} failed: {}",
            body["description"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| status.to_string())
        ),
        error_code: Some(body["error_code"].as_i64().unwrap_or(status)),
        retry_after: body["parameters"]["retry_after"]
            .as_i64()
            .filter(|n| *n > 0),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Bold,
    Italic,
    Code,
    Pre,
    Strikethrough,
    Spoiler,
    Blockquote,
    TextLink,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TextEntity {
    #[serde(rename = "type")]
    pub kind: EntityKind,
    pub offset: usize,
    pub length: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}
#[derive(Debug, Default)]
pub struct RenderedText {
    pub text: String,
    pub entities: Vec<TextEntity>,
}
enum EntityExtra<'a> {
    Url(&'a str),
    Language(&'a str),
}
#[derive(Default)]
struct Builder {
    text: String,
    length: usize,
    entities: Vec<TextEntity>,
}
impl Builder {
    fn add(&mut self, text: &str, kind: Option<EntityKind>, extra: Option<EntityExtra<'_>>) {
        if text.is_empty() {
            return;
        }
        let start = self.length;
        self.text.push_str(text);
        self.length += text.encode_utf16().count();
        if let Some(kind) = kind {
            self.wrap(start, kind, extra);
        }
    }
    fn wrap(&mut self, start: usize, kind: EntityKind, extra: Option<EntityExtra<'_>>) {
        if self.length <= start {
            return;
        }
        let mut entity = TextEntity {
            kind,
            offset: start,
            length: self.length - start,
            url: None,
            language: None,
        };
        match extra {
            Some(EntityExtra::Url(url)) => entity.url = Some(url.into()),
            Some(EntityExtra::Language(language)) => entity.language = Some(language.into()),
            None => {}
        }
        self.entities.push(entity);
    }
}
static LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\[([^\]\n]*)\]\(((?:https?|tg)://[^)\s]+)(?:\s+"[^"]*")?\)"#).unwrap()
});
static FENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[ \t]*(`{3,}|~{3,})[ \t]*([^\n`]*)$").unwrap());
static HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^#{1,6}\s+(.*)$").unwrap());
static BULLET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\s*)[-*+]\s+(.*)$").unwrap());
static NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\s*)(\d+)\.\s+(.*)$").unwrap());
static QUOTE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^>\s?(.*)$").unwrap());
fn word(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}
fn inline(builder: &mut Builder, src: &str, active: &HashSet<EntityKind>) {
    let mut i = 0;
    let mut plain = String::new();
    while i < src.len() {
        let tail = &src[i..];
        let Some(c) = tail.chars().next() else {
            break;
        };
        if c == '\\' {
            if let Some(next) = tail.chars().nth(1).filter(|c| c.is_ascii_punctuation()) {
                plain.push(next);
                i += 1 + next.len_utf8();
                continue;
            }
        }
        if let Some(link) = LINK.captures(tail) {
            builder.add(&plain, None, None);
            plain.clear();
            let start = builder.length;
            let mut nested = active.clone();
            nested.insert(EntityKind::TextLink);
            inline(builder, &link[1], &nested);
            builder.wrap(
                start,
                EntityKind::TextLink,
                Some(EntityExtra::Url(&link[2])),
            );
            i += link[0].len();
            continue;
        }
        let mut matched = false;
        for (marker, kind) in [
            ("```", EntityKind::Code),
            ("**", EntityKind::Bold),
            ("~~", EntityKind::Strikethrough),
            ("||", EntityKind::Spoiler),
            ("`", EntityKind::Code),
            ("*", EntityKind::Italic),
            ("_", EntityKind::Italic),
        ] {
            if !tail.starts_with(marker) || active.contains(&kind) {
                continue;
            }
            let intra = marker == "*" || marker == "_";
            if intra && word(src[..i].chars().next_back()) {
                continue;
            }
            let start = i + marker.len();
            let Some(relative) = src[start..].find(marker) else {
                continue;
            };
            let end = start + relative;
            if start == end {
                continue;
            }
            if intra && word(src[end + marker.len()..].chars().next()) {
                continue;
            }
            let inner = &src[start..end];
            if kind != EntityKind::Code
                && (inner.starts_with(char::is_whitespace) || inner.ends_with(char::is_whitespace))
            {
                continue;
            }
            builder.add(&plain, None, None);
            plain.clear();
            if kind == EntityKind::Code {
                builder.add(
                    inner,
                    (!active.contains(&EntityKind::TextLink)).then_some(EntityKind::Code),
                    None,
                );
            } else {
                let offset = builder.length;
                let mut nested = active.clone();
                nested.insert(kind);
                inline(builder, inner, &nested);
                builder.wrap(offset, kind, None);
            }
            i = end + marker.len();
            matched = true;
            break;
        }
        if !matched {
            plain.push(c);
            i += c.len_utf8();
        }
    }
    builder.add(&plain, None, None);
}
fn line(builder: &mut Builder, src: &str) {
    if let Some(v) = HEADING.captures(src) {
        let start = builder.length;
        inline(builder, &v[1], &HashSet::new());
        builder.wrap(start, EntityKind::Bold, None);
    } else if let Some(v) = BULLET.captures(src) {
        builder.add(&format!("{}• ", &v[1]), None, None);
        inline(builder, &v[2], &HashSet::new());
    } else if let Some(v) = NUMBER.captures(src) {
        builder.add(&format!("{}{}. ", &v[1], &v[2]), None, None);
        inline(builder, &v[3], &HashSet::new());
    } else {
        inline(builder, src, &HashSet::new());
    }
}
fn block(builder: &mut Builder, lines: &[&str]) {
    let mut quote_start = None;
    for (n, src) in lines.iter().enumerate() {
        let quote = QUOTE.captures(src);
        if quote.is_none() {
            if let Some(start) = quote_start.take() {
                builder.wrap(start, EntityKind::Blockquote, None);
            }
        }
        if n > 0 {
            builder.add("\n", None, None);
        }
        if let Some(v) = quote {
            quote_start.get_or_insert(builder.length);
            inline(builder, &v[1], &HashSet::new());
        } else {
            line(builder, src);
        }
    }
    if let Some(start) = quote_start {
        builder.wrap(start, EntityKind::Blockquote, None);
    }
}
pub fn render(markdown: &str) -> RenderedText {
    let mut b = Builder::default();
    let normalized = markdown.replace("\r\n", "\n");
    let lines: Vec<_> = normalized.split('\n').collect();
    let mut i = 0;
    while i < lines.len() {
        if let Some(fence) = FENCE.captures(lines[i]) {
            let closer = if fence[1].starts_with('`') {
                "```"
            } else {
                "~~~"
            };
            let language = fence[2].split_whitespace().next();
            let mut body = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with(closer) {
                body.push(lines[i]);
                i += 1;
            }
            i += 1;
            let body = body.join("\n");
            let code = body.trim_end_matches('\n');
            if !code.is_empty() {
                if b.length > 0 {
                    b.add("\n\n", None, None);
                }
                b.add(
                    code,
                    Some(EntityKind::Pre),
                    language.map(EntityExtra::Language),
                );
            }
            continue;
        }
        if lines[i].trim().is_empty() {
            i += 1;
            continue;
        }
        let start = i;
        while i < lines.len() && !lines[i].trim().is_empty() && !FENCE.is_match(lines[i]) {
            i += 1;
        }
        if b.length > 0 {
            b.add("\n\n", None, None);
        }
        block(&mut b, &lines[start..i]);
    }
    let mut indexed: Vec<_> = b.entities.into_iter().enumerate().collect();
    indexed.sort_by_key(|(n, e)| (e.offset, std::cmp::Reverse(e.length), std::cmp::Reverse(*n)));
    RenderedText {
        text: b.text,
        entities: indexed.into_iter().map(|(_, e)| e).collect(),
    }
}
pub fn text_payload(content: Option<&str>) -> Result<Value, &'static str> {
    let content = content
        .filter(|s| !s.trim().is_empty())
        .ok_or("Telegram replies require nonempty text")?;
    let rendered = render(content);
    if rendered.text.trim().is_empty() {
        return Err("Telegram replies require nonempty text");
    }
    if rendered.text.encode_utf16().count() > 4096 {
        return Err("Telegram replies must fit 4,096 characters; send separate replies");
    }
    let mut payload = json!({"text":rendered.text,"link_preview_options":{"is_disabled":true}});
    if !rendered.entities.is_empty() {
        payload["entities"] = json!(rendered.entities);
    }
    Ok(payload)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utf16_entities_and_mentions() {
        let r = render("😀 **bold 😀**");
        assert_eq!(
            serde_json::to_value(&r.entities[0]).unwrap(),
            json!({"type":"bold","offset":3,"length":7})
        );
        let a = strip_bot_address(
            "😀 @bot hi",
            &[json!({"type":"mention","offset":3,"length":4})],
            1,
            "bot",
        );
        assert!(a.is_addressed);
        assert_eq!(a.text, "😀  hi");
    }
    #[test]
    fn malformed_utf16_and_expanding_casefold_are_safe() {
        let address = strip_bot_address(
            "@İ",
            &[json!({"type":"bot_command","offset":0,"length":2})],
            1,
            "İ",
        );
        assert!(address.is_addressed);
        assert_eq!(address.text, "@İ");
        let address = strip_bot_address(
            "😀 @bot",
            &[
                json!({"type":"mention","offset":1,"length":1}),
                json!({"type":"mention","offset":u64::MAX,"length":1}),
            ],
            1,
            "bot",
        );
        assert!(!address.is_addressed);
        assert_eq!(address.text, "😀 @bot");
    }
    #[test]
    fn validated_routes_and_limits() {
        assert_eq!(validate_config("99:abc", None, None).unwrap(), 99);
        assert!(validate_config("99:abc", Some("http://x/webhooks/telegram"), Some("x")).is_err());
        assert_eq!(truncate_caption(&"😀".repeat(600)), "😀".repeat(512));
        assert!(text_payload(Some(&"😀".repeat(2049))).is_err());
    }
    #[test]
    fn nested_markup() {
        assert_eq!(render("**bold _it_ end**").text, "bold it end");
        assert_eq!(render("snake_case_name").entities.len(), 0);
        assert_eq!(
            render("[**big** docs](https://x.y)").entities[0].kind,
            EntityKind::TextLink
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotIdentity {
    pub username: String,
    pub first_name: String,
}
pub fn bot_identity(response: &Value, expected_bot: i64) -> Result<BotIdentity, &'static str> {
    let username =
        nonempty(&response["username"]).ok_or("Telegram did not return the bot identity")?;
    if response["id"].as_i64() != Some(expected_bot) {
        return Err("Telegram token does not match its bot identity");
    }
    Ok(BotIdentity {
        username: username.into(),
        first_name: response["first_name"].as_str().unwrap_or(username).into(),
    })
}
pub fn rich_payload(content: &agentchat_core::RichReply) -> Result<Value, &'static str> {
    if !content.attachments.is_empty() {
        return Err("Telegram rich replies do not support attachments");
    }
    if content.blocks.len() != 1 {
        return Err("Telegram rich replies need exactly one reply_markup mapping");
    }
    let mut payload = text_payload(Some(&content.text))?;
    payload["reply_markup"] = json!(content.blocks[0]);
    Ok(payload)
}
/// A prompt as a message with one inline button per choice: one row for up to
/// three, otherwise one per row.
pub fn prompt_payload(
    text: &str,
    prompt: &str,
    choices: &[(String, String)],
) -> Result<Value, &'static str> {
    let mut payload = text_payload(Some(text))?;
    let buttons = choices
        .iter()
        .map(|(id, label)| {
            agentchat_core::prompt_token(prompt, id)
                .map(|token| json!({"text": label, "callback_data": token}))
                .map_err(|_| "Invalid prompt or choice ID")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let rows: Vec<Value> = if buttons.len() <= 3 {
        vec![json!(buttons)]
    } else {
        buttons.into_iter().map(|b| json!([b])).collect()
    };
    payload["reply_markup"] = json!({"inline_keyboard": rows});
    Ok(payload)
}
pub fn validate_upload(file: &agentchat_core::UploadFile) -> Result<(), &'static str> {
    if !plain_filename(&file.filename) {
        return Err("Expected a plain filename");
    }
    if file.content.is_empty() {
        return Err("Expected nonempty file bytes");
    }
    Ok(())
}
pub fn upload_fields(destination: &Destination, title: Option<&str>) -> Value {
    let mut fields = destination.route(json!({}), false);
    if let Some(title) = title.filter(|s| !s.is_empty()) {
        fields["caption"] = json!(truncate_caption(title));
    }
    fields
}
pub fn document_receipt(result: &Value) -> Result<String, &'static str> {
    nonempty(&result["document"]["file_id"])
        .map(str::to_owned)
        .ok_or("Telegram did not return a document receipt")
}
pub fn validate_download(
    source: &Message,
    bot: i64,
    attachment: &Attachment,
    limit: &Value,
) -> Result<usize, &'static str> {
    if source.role != Role::User {
        return Err("Source does not belong to this Telegram adapter");
    }
    destination(source, bot)?;
    if !source.attachments.contains(attachment) {
        return Err("Attachment does not belong to the source message");
    }
    let limit = limit
        .as_u64()
        .filter(|n| *n > 0)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or("Expected a positive attachment byte limit")?;
    Ok(limit)
}
pub fn download_name(attachment: &Attachment, path: &str) -> String {
    attachment
        .name
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            path.rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("attachment")
        })
        .into()
}

/// The Bot API method and payload that close a prompt: its text with the answer, or
/// (`None`, on expiry) the same message without its keyboard.
pub fn close_prompt_request(answered: Option<&str>) -> Result<(&'static str, Value), &'static str> {
    match answered {
        Some(text) => Ok(("editMessageText", text_payload(Some(text))?)),
        None => Ok((
            "editMessageReplyMarkup",
            json!({"reply_markup": {"inline_keyboard": []}}),
        )),
    }
}

/// The kind of an update: its one field besides `update_id`, such as `message`,
/// `callback_query` or `my_chat_member`.
pub fn update_kind(update: &Value) -> Option<&str> {
    update
        .as_object()?
        .keys()
        .find(|key| key.as_str() != "update_id")
        .map(String::as_str)
}

/// Where an update goes when the application has a handler for it: a press that
/// is not a prompt answer is an action (by its `callback_data`), a message starting
/// with `/name` is a command, and any other update is an event by its kind.
/// Without a matching handler, presses and messages keep their usual path.
pub fn route(update: &Value) -> Option<agentchat_core::Route> {
    use agentchat_core::Route;
    let kind = update_kind(update)?;
    match kind {
        "callback_query" => {
            let data = nonempty(&update["callback_query"]["data"])?;
            if agentchat_core::answer_metadata(data).is_some() {
                return None;
            }
            Some(Route::Action {
                action_ids: vec![data.into()],
            })
        }
        "message" => {
            let text = update["message"]["text"].as_str()?;
            agentchat_core::command_of(text).map(|command| Route::Command { command })
        }
        _ => Some(Route::Event {
            event_type: kind.into(),
        }),
    }
}

/// The update types to request: messages and presses, the event types the
/// application handles, and stop presses on drafts when drafts are streamed.
pub fn allowed_updates(events: &[String], drafts: bool) -> Vec<String> {
    let mut allowed = vec!["message".to_owned(), "callback_query".to_owned()];
    if drafts {
        allowed.push("stopped_message_generation".into());
    }
    for event in events {
        if !allowed.contains(event) {
            allowed.push(event.clone());
        }
    }
    allowed
}

/// The stop button on a streamed draft (`stopped_message_generation`), as a stop
/// request for that private chat's conversation, sent by its user.
pub fn normalize_stop(update: &Value, bot: i64) -> Option<Message> {
    let stopped = &update["stopped_message_generation"];
    let chat = &stopped["chat"];
    let chat_id = chat["id"].as_i64()?;
    if chat["type"] != "private" {
        return None;
    }
    let draft = stopped["draft_id"].as_i64()?;
    let base = format!("telegram:{bot}:{chat_id}");
    let thread = stopped["message_thread_id"].as_i64();
    let mut metadata: serde_json::Map<String, Value> = [
        ("bot_id".into(), json!(bot)),
        ("chat_id".into(), json!(chat_id)),
        ("chat_type".into(), json!("private")),
        ("message_id".into(), json!(0)),
        ("message_thread_id".into(), json!(thread)),
        ("draft_id".into(), json!(draft)),
    ]
    .into_iter()
    .collect();
    metadata.insert(agentchat_core::STOP_METADATA.into(), json!(true));
    Some(Message {
        id: format!(
            "telegram:{bot}:stop:{chat_id}:{draft}:{}",
            update["update_id"]
        )
        .into(),
        conversation_id: thread.map(|v| format!("{base}:{v}")).unwrap_or(base).into(),
        channel: "telegram".into(),
        sender: Sender {
            id: chat_id.to_string(),
            display_name: None,
        },
        text: String::new(),
        role: Role::User,
        thread_id: thread.map(|v| v.to_string()),
        addressed: true,
        attachments: vec![],
        metadata,
    })
}

/// The `answerCallbackQuery` request for a press the application handled: its
/// answer may set `text` (up to 200 characters), `show_alert`, `url` and
/// `cache_time`; anything else is ignored.
pub fn callback_answer(callback_id: &str, answer: &Value) -> Value {
    let mut request = json!({"callback_query_id": callback_id});
    for key in ["text", "show_alert", "url", "cache_time"] {
        if let Some(value) = answer.get(key).filter(|value| !value.is_null()) {
            request[key] = value.clone();
        }
    }
    request
}

/// How a private notice is sent in the source chat.
#[derive(Debug, Clone, PartialEq)]
pub enum Ephemeral {
    /// A private chat with that user: an ordinary reply is already private.
    Reply(Value),
    /// A group: an ephemeral `sendMessage` only that user sees.
    Ephemeral(Value),
}

/// A notice only `user_id` sees. In a group, Telegram accepts it within 15 seconds
/// of that user's button press (`callback_query_id`), or at any time when the bot
/// is an administrator.
pub fn ephemeral_request(
    source: &Message,
    bot: i64,
    user_id: &str,
    text: &str,
    reply_markup: Option<&Value>,
) -> Result<Ephemeral, &'static str> {
    let user: i64 = user_id
        .parse()
        .map_err(|_| "Expected a numeric Telegram user ID")?;
    let dest = destination(source, bot)?;
    let mut payload = text_payload(Some(text))?;
    if let Some(markup) = reply_markup {
        payload["reply_markup"] = markup.clone();
    }
    let private = source.metadata.get("chat_type").and_then(Value::as_str) == Some("private");
    if private {
        if dest.chat_id != user {
            return Err("A private Telegram chat can only show notices to its own user");
        }
        return Ok(Ephemeral::Reply(dest.route(payload, true)));
    }
    let mut parameters = json!({"receiver_user_id": user});
    if let Some(callback) = source
        .metadata
        .get("callback_query_id")
        .and_then(Value::as_str)
    {
        parameters["callback_query_id"] = json!(callback);
    }
    payload["ephemeral_message_parameters"] = parameters;
    Ok(Ephemeral::Ephemeral(dest.route(payload, false)))
}

/// The receipt of a group notice; Telegram identifies it by `ephemeral_message_id`.
pub fn ephemeral_receipt(
    source: &Message,
    bot: i64,
    name: Option<String>,
    user_id: &str,
    text: &str,
    result: &Value,
) -> Result<Message, &'static str> {
    let id = result["ephemeral_message_id"]
        .as_i64()
        .filter(|id| *id > 0)
        .ok_or("Telegram did not return an ephemeral message receipt")?;
    let mut sent = receipt(source, bot, name, text, &json!({"message_id": id}))?;
    sent.id = format!("{}:ephemeral", sent.id).into();
    // Notices have no ordinary message ID; they cannot be quoted or edited as one.
    sent.metadata.insert("message_id".into(), json!(0));
    sent.metadata
        .insert("ephemeral_message_id".into(), json!(id));
    sent.metadata.insert("ephemeral".into(), json!(true));
    sent.metadata
        .insert("recipient_user_id".into(), json!(user_id));
    Ok(sent)
}

/// `deleteMessage`, or `deleteEphemeralMessage` for a notice, for a message this
/// bot sent. Telegram deletes messages up to 48 hours old.
pub fn delete_request(message: &Message, bot: i64) -> Result<(&'static str, Value), &'static str> {
    let dest = destination(message, bot)?;
    if message.metadata.get("ephemeral") == Some(&json!(true)) {
        let id = message
            .metadata
            .get("ephemeral_message_id")
            .and_then(Value::as_i64)
            .ok_or("Expected a Telegram message to delete")?;
        let user: i64 = message
            .metadata
            .get("recipient_user_id")
            .and_then(Value::as_str)
            .and_then(|user| user.parse().ok())
            .ok_or("Expected a Telegram message to delete")?;
        return Ok((
            "deleteEphemeralMessage",
            json!({"chat_id": dest.chat_id, "receiver_user_id": user, "ephemeral_message_id": id}),
        ));
    }
    if dest.message_id <= 0 {
        return Err("Expected a Telegram message to delete");
    }
    Ok((
        "deleteMessage",
        json!({"chat_id": dest.chat_id, "message_id": dest.message_id}),
    ))
}

/// Whether answers in this chat stream as drafts: private chats only.
pub fn drafts_allowed(source: &Message) -> bool {
    source.metadata.get("chat_type").and_then(Value::as_str) == Some("private")
}

/// `sendMessageDraft` showing `text` with a stop button; empty text shows Telegram's
/// "Thinking…" placeholder.
pub fn draft_request(dest: &Destination, draft_id: i64, text: &str) -> Value {
    let mut payload = if text.trim().is_empty() {
        json!({"text": ""})
    } else {
        text_payload(Some(text)).unwrap_or_else(|_| json!({"text": ""}))
    };
    payload["chat_id"] = json!(dest.chat_id);
    if let Some(thread) = dest.thread_id {
        payload["message_thread_id"] = json!(thread);
    }
    payload["draft_id"] = json!(draft_id);
    payload["can_stop"] = json!(true);
    payload
}

#[cfg(test)]
mod app_tests {
    use super::*;
    use agentchat_core::Route;

    fn source(chat_type: &str, chat: i64) -> Message {
        let update = json!({"update_id": 1, "message": {"message_id": 7,
            "chat": {"id": chat, "type": chat_type}, "from": {"id": 5, "is_bot": false, "first_name": "A"},
            "text": "@bot hi", "entities": [{"type": "mention", "offset": 0, "length": 4}]}});
        normalize(&update, 99, "bot").unwrap()
    }

    #[test]
    fn routes_presses_commands_and_other_updates() {
        let press = json!({"update_id": 1, "callback_query": {"id": "c", "data": "retry"}});
        assert_eq!(
            route(&press),
            Some(Route::Action {
                action_ids: vec!["retry".into()]
            })
        );
        let token = agentchat_core::prompt_token("0123456789abcdef", "yes").unwrap();
        assert_eq!(
            route(&json!({"update_id": 1, "callback_query": {"id": "c", "data": token}})),
            None
        );
        assert_eq!(
            route(&json!({"update_id": 1, "message": {"text": "/start@bot go"}})),
            Some(Route::Command {
                command: "/start".into()
            })
        );
        assert_eq!(
            route(&json!({"update_id": 1, "message": {"text": "hello"}})),
            None
        );
        assert_eq!(
            route(&json!({"update_id": 1, "my_chat_member": {}})),
            Some(Route::Event {
                event_type: "my_chat_member".into()
            })
        );
        assert_eq!(
            allowed_updates(&["my_chat_member".into(), "message".into()], true),
            [
                "message",
                "callback_query",
                "stopped_message_generation",
                "my_chat_member"
            ]
        );
    }

    #[test]
    fn stop_presses_stop_the_private_chat() {
        let update = json!({"update_id": 9, "stopped_message_generation": {
            "chat": {"id": 5, "type": "private"}, "draft_id": 42}});
        let stop = normalize_stop(&update, 99).unwrap();
        assert!(agentchat_core::is_stop_request(&stop));
        assert_eq!(stop.conversation_id.as_ref(), "telegram:99:5");
        assert_eq!(stop.sender.id, "5");
        let mut group = update;
        group["stopped_message_generation"]["chat"]["type"] = json!("group");
        assert!(normalize_stop(&group, 99).is_none());
    }

    #[test]
    fn notices_are_private_replies_or_ephemeral_group_messages() {
        let private = source("private", 5);
        assert!(matches!(
            ephemeral_request(&private, 99, "5", "hi", None),
            Ok(Ephemeral::Reply(_))
        ));
        assert!(ephemeral_request(&private, 99, "6", "hi", None).is_err());
        let group = source("group", -100);
        let Ok(Ephemeral::Ephemeral(request)) = ephemeral_request(&group, 99, "6", "hi", None)
        else {
            panic!("expected an ephemeral group message");
        };
        assert_eq!(
            request["ephemeral_message_parameters"]["receiver_user_id"],
            6
        );
        assert_eq!(request["chat_id"], -100);
        assert!(ephemeral_request(&group, 99, "U6", "hi", None).is_err());
        let notice = ephemeral_receipt(
            &group,
            99,
            None,
            "6",
            "hi",
            &json!({"ephemeral_message_id": 3}),
        )
        .unwrap();
        assert_eq!(
            delete_request(&notice, 99).unwrap(),
            (
                "deleteEphemeralMessage",
                json!({"chat_id": -100, "receiver_user_id": 6, "ephemeral_message_id": 3})
            )
        );
        assert_eq!(
            delete_request(&group, 99).unwrap(),
            ("deleteMessage", json!({"chat_id": -100, "message_id": 7}))
        );
        assert_eq!(
            callback_answer(
                "c",
                &json!({"text": "Saved", "show_alert": true, "other": 1})
            ),
            json!({"callback_query_id": "c", "text": "Saved", "show_alert": true})
        );
        let dest = destination(&private, 99).unwrap();
        let draft = draft_request(&dest, 42, "");
        assert_eq!(
            (
                draft["draft_id"].clone(),
                draft["can_stop"].clone(),
                draft["text"].clone()
            ),
            (json!(42), json!(true), json!(""))
        );
        assert!(drafts_allowed(&private) && !drafts_allowed(&group));
    }
}
