//! Discord message selection, limits, and confirmed reply routing.
use crate::wire::strip;
use agentchat_core::{Attachment, Message, Role, Sender};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct Event {
    pub id: u64,
    pub channel_id: u64,
    pub guild_id: Option<u64>,
    pub author_id: u64,
    pub display_name: Option<String>,
    pub author_bot: bool,
    pub webhook: bool,
    pub system: bool,
    pub mentions: Vec<u64>,
    pub replied_author: Option<u64>,
    pub is_thread: bool,
    pub content: String,
    pub attachments: Vec<Attachment>,
}
pub fn normalize(event: Event, bot_id: u64) -> Option<Message> {
    if event.author_bot || event.webhook || event.system || event.author_id == bot_id {
        return None;
    }
    if event.guild_id.is_some()
        && !event.mentions.contains(&bot_id)
        && event.replied_author != Some(bot_id)
    {
        return None;
    }
    let text = event
        .content
        .replace(&format!("<@{bot_id}>"), "")
        .replace(&format!("<@!{bot_id}>"), "");
    let text = strip(&text).to_owned();
    if text.is_empty() && event.attachments.is_empty() {
        return None;
    }
    Some(Message {
        id: format!("discord:{bot_id}:{}:{}", event.channel_id, event.id).into(),
        conversation_id: conversation(bot_id, event.guild_id, event.channel_id).into(),
        channel: "discord".into(),
        sender: Sender {
            id: event.author_id.to_string(),
            display_name: event.display_name,
        },
        text,
        role: Role::User,
        metadata: json!({
            "bot_id": bot_id,
            "channel_id": event.channel_id,
            "guild_id": event.guild_id,
            "message_id": event.id,
        })
        .as_object()
        .unwrap()
        .clone(),
        thread_id: event.is_thread.then(|| event.channel_id.to_string()),
        addressed: true,
        attachments: event.attachments,
    })
}
/// A button press, from a Discord component interaction.
#[derive(Debug, Clone)]
pub struct Press {
    pub id: u64,
    pub channel_id: u64,
    pub guild_id: Option<u64>,
    pub user_id: u64,
    pub display_name: Option<String>,
    pub user_bot: bool,
    pub custom_id: String,
    pub message_id: u64,
    pub is_thread: bool,
}
fn conversation(bot_id: u64, guild_id: Option<u64>, channel_id: u64) -> String {
    format!(
        "discord:{bot_id}:{}:{channel_id}",
        guild_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "dm".into()),
    )
}
/// A press on one of this bot's prompt buttons, as a message in the prompt's
/// conversation; None for anything else.
pub fn normalize_press(press: Press, bot_id: u64) -> Option<Message> {
    if press.user_bot || press.user_id == bot_id {
        return None;
    }
    let answer = agentchat_core::answer_metadata(&press.custom_id)?;
    let mut metadata = json!({
        "bot_id": bot_id,
        "channel_id": press.channel_id,
        "guild_id": press.guild_id,
        "message_id": press.message_id,
    });
    metadata[agentchat_core::ANSWER_METADATA] = answer;
    Some(Message {
        id: format!(
            "discord:{bot_id}:{}:interaction:{}",
            press.channel_id, press.id
        )
        .into(),
        conversation_id: conversation(bot_id, press.guild_id, press.channel_id).into(),
        channel: "discord".into(),
        sender: Sender {
            id: press.user_id.to_string(),
            display_name: press.display_name,
        },
        text: String::new(),
        role: Role::User,
        metadata: metadata.as_object().unwrap().clone(),
        thread_id: press.is_thread.then(|| press.channel_id.to_string()),
        addressed: true,
        attachments: vec![],
    })
}
pub fn text_payload(content: &str) -> Result<&str, &'static str> {
    if strip(content).is_empty() {
        return Err("Discord replies require nonempty text");
    }
    if content.encode_utf16().count() > 2000 {
        return Err("Discord replies must fit 2,000 characters; send separate replies");
    }
    Ok(content)
}
#[derive(Debug, Clone)]
pub struct Destination {
    pub bot_id: u64,
    pub channel_id: u64,
    pub message_id: u64,
    pub guild_id: Option<u64>,
}
impl Destination {
    pub fn from_source(source: &Message, bot_id: u64) -> Result<Self, &'static str> {
        let data = &source.metadata;
        if source.channel != "discord" || data.get("bot_id").and_then(Value::as_u64) != Some(bot_id)
        {
            return Err("Source does not belong to this Discord adapter");
        }
        Ok(Self {
            bot_id,
            channel_id: data
                .get("channel_id")
                .and_then(Value::as_u64)
                .ok_or("Source does not belong to this Discord adapter")?,
            message_id: data
                .get("message_id")
                .and_then(Value::as_u64)
                .ok_or("Source does not belong to this Discord adapter")?,
            guild_id: data.get("guild_id").and_then(Value::as_u64),
        })
    }
    pub fn payload(&self, content: &str) -> Value {
        let mut reference = json!({"message_id":self.message_id.to_string(),"channel_id":self.channel_id.to_string(),"fail_if_not_exists":false});
        if let Some(guild) = self.guild_id {
            reference["guild_id"] = guild.to_string().into();
        }
        json!({"content":content,"message_reference":reference,"allowed_mentions":{"parse":[],"replied_user":false}})
    }
    /// An edit replacing a sent message's text; `components: []` removes its buttons.
    pub fn edit_payload(content: &str, remove_buttons: bool) -> Value {
        let mut payload =
            json!({"content":content,"allowed_mentions":{"parse":[],"replied_user":false}});
        if remove_buttons {
            payload["components"] = json!([]);
        }
        payload
    }
    /// A prompt as a reply with buttons, five to a row.
    pub fn prompt_payload(
        &self,
        text: &str,
        prompt: &str,
        choices: &[(String, String, String)],
    ) -> Result<Value, &'static str> {
        let mut payload = self.payload(text_payload(text)?);
        let buttons = choices
            .iter()
            .map(|(id, label, style)| {
                let style = match style.as_str() {
                    "primary" => 1,
                    "danger" => 4,
                    _ => 2,
                };
                agentchat_core::prompt_token(prompt, id)
                    .map(|token| json!({"type": 2, "style": style, "label": label, "custom_id": token}))
                    .map_err(|_| "Invalid prompt or choice ID")
            })
            .collect::<Result<Vec<_>, _>>()?;
        payload["components"] = buttons
            .chunks(5)
            .map(|row| json!({"type": 1, "components": row}))
            .collect();
        Ok(payload)
    }
    pub fn confirmed_reply(
        &self,
        source: &Message,
        content: String,
        receipt: u64,
        display_name: Option<String>,
    ) -> Result<Message, &'static str> {
        if receipt == 0 {
            return Err("Discord did not return a message receipt");
        }
        let mut result = source.clone();
        result.id = format!("discord:{}:{}:{receipt}", self.bot_id, self.channel_id).into();
        result.sender = Sender {
            id: self.bot_id.to_string(),
            display_name,
        };
        result.text = content;
        result.role = Role::Assistant;
        result.addressed = true;
        result.attachments.clear();
        result.metadata.insert("message_id".into(), receipt.into());
        Ok(result)
    }
}
pub fn receipt(response: &Value) -> Result<u64, &'static str> {
    let raw = response
        .get("id")
        .and_then(Value::as_str)
        .ok_or("Discord did not return a message receipt")?;
    if raw.is_empty() || !raw.bytes().all(|ch| ch.is_ascii_digit()) {
        return Err("Discord did not return a message receipt");
    }
    raw.parse()
        .ok()
        .filter(|id| *id > 0)
        .ok_or("Discord did not return a message receipt")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_uses_utf16() {
        assert!(text_payload(&"😀".repeat(1000)).is_ok());
        assert!(text_payload(&"😀".repeat(1001)).is_err());
    }
    #[test]
    fn receipts_are_confirmed() {
        assert_eq!(
            receipt(&json!({"id":"9007199254740993"})).unwrap(),
            9007199254740993
        );
        assert!(receipt(&json!({"id":"0"})).is_err());
    }

    #[test]
    fn selected_thread_message_has_one_validated_reply_route() {
        let event = Event {
            id: 9007199254740993,
            channel_id: 10,
            guild_id: Some(20),
            author_id: 30,
            display_name: Some("雪".into()),
            author_bot: false,
            webhook: false,
            system: false,
            mentions: vec![99],
            replied_author: None,
            is_thread: true,
            content: "\u{1c}<@99> hi <@!99> <@42>\u{1f}".into(),
            attachments: vec![],
        };
        let message = normalize(event.clone(), 99).unwrap();
        assert_eq!(message.id.as_ref(), "discord:99:10:9007199254740993");
        assert_eq!(message.text, "hi  <@42>");
        assert_eq!(message.thread_id.as_deref(), Some("10"));
        let route = Destination::from_source(&message, 99).unwrap();
        assert_eq!(
            route.payload("answer")["message_reference"]["message_id"],
            "9007199254740993"
        );
        let response = route
            .confirmed_reply(&message, "answer".into(), 123, Some("Bot".into()))
            .unwrap();
        assert_eq!(response.conversation_id, message.conversation_id);
        assert_eq!(response.metadata["message_id"], 123);
        assert!(Destination::from_source(&message, 100).is_err());
        let mut ignored = event;
        ignored.mentions.clear();
        assert!(normalize(ignored, 99).is_none());
    }
}
