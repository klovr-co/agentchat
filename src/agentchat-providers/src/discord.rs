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
        // A direct source (see `direct_source`) has no message to reply to.
        if self.message_id == 0 {
            return json!({"content":content,"allowed_mentions":{"parse":[],"replied_user":false}});
        }
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
/// Interaction types, from Discord's interaction object.
pub const APPLICATION_COMMAND: u64 = 2;
pub const MESSAGE_COMPONENT: u64 = 3;
pub const AUTOCOMPLETE: u64 = 4;
pub const MODAL_SUBMIT: u64 = 5;

/// Where an interaction goes: a component press that is not a prompt answer is an
/// action (by `custom_id`), a modal submission a view (by `custom_id`), an
/// application command a command (`/name`), and autocomplete anything else.
pub fn interaction_route(kind: u64, data: &Value) -> Option<agentchat_core::Route> {
    use agentchat_core::Route;
    let custom_id = || data["custom_id"].as_str().filter(|id| !id.is_empty());
    match kind {
        MESSAGE_COMPONENT => {
            let id = custom_id()?;
            if agentchat_core::answer_metadata(id).is_some() {
                return None;
            }
            Some(Route::Action {
                action_ids: vec![id.into()],
            })
        }
        MODAL_SUBMIT => Some(Route::View {
            callback_id: custom_id()?.into(),
        }),
        APPLICATION_COMMAND => Some(Route::Command {
            command: format!("/{}", data["name"].as_str().filter(|n| !n.is_empty())?),
        }),
        AUTOCOMPLETE => Some(Route::Other),
        _ => None,
    }
}

/// The interaction response to send: the handler's answer when it is a response
/// object (it has a `type`), otherwise an acknowledgement that shows nothing new.
/// Commands get a private "thinking" state the handler can follow up on.
pub fn interaction_response(kind: u64, answer: &Value) -> Value {
    if answer.get("type").and_then(Value::as_u64).is_some() {
        return answer.clone();
    }
    match kind {
        APPLICATION_COMMAND => json!({"type": 5, "data": {"flags": 64}}),
        AUTOCOMPLETE => json!({"type": 8, "data": {"choices": []}}),
        _ => json!({"type": 6}),
    }
}

/// A dispatched gateway event (opcode 0) as its type and the raw frame.
pub fn gateway_event(frame: &Value) -> Option<&str> {
    if frame["op"] != 0 {
        return None;
    }
    frame["t"].as_str().filter(|t| !t.is_empty())
}

/// The REST path that deletes a message this bot sent.
pub fn delete_path(message: &Message, bot_id: u64) -> Result<String, &'static str> {
    let destination = Destination::from_source(message, bot_id)?;
    Ok(format!(
        "channels/{}/messages/{}",
        destination.channel_id, destination.message_id
    ))
}

/// Whether a notice for `user_id` can be an ordinary reply: only in a direct
/// message with that user, where every message is already private.
/// `POST users/@me/channels` with this body opens (or finds) the direct message
/// channel with `user_id`.
pub fn dm_request(user_id: &str) -> Result<Value, &'static str> {
    if user_id.is_empty() || !user_id.bytes().all(|c| c.is_ascii_digit()) {
        return Err("Expected a Discord user ID");
    }
    Ok(json!({"recipient_id": user_id}))
}

/// The channel ID in a `users/@me/channels` response.
pub fn dm_channel(response: &Value) -> Result<u64, &'static str> {
    receipt(response).map_err(|_| "Discord did not return a direct message channel")
}

/// A source for messages sent to the direct message `channel_id` with `user_id`,
/// which need not be answering one of theirs.
pub fn direct_source(bot_id: u64, channel_id: u64, user_id: &str) -> Message {
    Message {
        id: format!("discord:{bot_id}:{channel_id}:direct").into(),
        conversation_id: conversation(bot_id, None, channel_id).into(),
        channel: "discord".into(),
        sender: Sender {
            id: user_id.into(),
            display_name: None,
        },
        text: String::new(),
        role: Role::User,
        metadata: json!({
            "bot_id": bot_id,
            "channel_id": channel_id,
            "guild_id": null,
            "message_id": 0,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
        thread_id: None,
        addressed: true,
        attachments: Vec::new(),
    }
}

/// `GET` lists a channel's pins (`{"items": [{"message": ...}], "has_more"}`);
/// `PUT /{message_id}` pins one.
pub fn pins_path(channel_id: u64) -> String {
    format!("channels/{channel_id}/messages/pins")
}

/// The newest pinned message the bot wrote, in a `pins_path` listing.
pub fn pinned_home(pins: &Value, bot_id: u64) -> Option<u64> {
    let bot = bot_id.to_string();
    pins["items"].as_array()?.iter().find_map(|pin| {
        let message = &pin["message"];
        if message["author"]["id"].as_str() != Some(bot.as_str()) {
            return None;
        }
        receipt(message).ok()
    })
}

pub fn private_reply_allowed(source: &Message, user_id: &str) -> bool {
    source.metadata.get("guild_id").is_none_or(Value::is_null) && source.sender.id == user_id
}

#[cfg(test)]
mod tests {
    use agentchat_core::Route;

    #[test]
    fn direct_messages_and_pinned_homes() {
        let source = direct_source(99, 5, "7");
        let destination = Destination::from_source(&source, 99).unwrap();
        assert!(destination.payload("hi").get("message_reference").is_none());
        assert!(private_reply_allowed(&source, "7"));
        assert_eq!(dm_request("7").unwrap(), json!({"recipient_id": "7"}));
        assert!(dm_request("x").is_err());
        assert_eq!(dm_channel(&json!({"id": "5"})), Ok(5));
        let pins = json!({"items": [
            {"message": {"id": "11", "author": {"id": "3"}}},
            {"message": {"id": "12", "author": {"id": "99"}}}]});
        assert_eq!(pinned_home(&pins, 99), Some(12));
        assert_eq!(pinned_home(&json!({"items": []}), 99), None);
        assert_eq!(pins_path(5), "channels/5/messages/pins");
    }

    #[test]
    fn interactions_route_by_type_and_get_a_response() {
        assert_eq!(
            interaction_route(MESSAGE_COMPONENT, &json!({"custom_id": "retry"})),
            Some(Route::Action {
                action_ids: vec!["retry".into()]
            })
        );
        let token = agentchat_core::prompt_token("0123456789abcdef", "yes").unwrap();
        assert_eq!(
            interaction_route(MESSAGE_COMPONENT, &json!({"custom_id": token})),
            None
        );
        assert_eq!(
            interaction_route(MODAL_SUBMIT, &json!({"custom_id": "settings"})),
            Some(Route::View {
                callback_id: "settings".into()
            })
        );
        assert_eq!(
            interaction_route(APPLICATION_COMMAND, &json!({"name": "deploy"})),
            Some(Route::Command {
                command: "/deploy".into()
            })
        );
        assert_eq!(interaction_route(1, &json!({})), None);
        let modal = json!({"type": 9, "data": {"custom_id": "m"}});
        assert_eq!(interaction_response(MESSAGE_COMPONENT, &modal), modal);
        assert_eq!(
            interaction_response(MESSAGE_COMPONENT, &Value::Null),
            json!({"type": 6})
        );
        assert_eq!(
            interaction_response(APPLICATION_COMMAND, &json!({"text": "x"}))["type"],
            5
        );
        assert_eq!(
            gateway_event(&json!({"op": 0, "t": "GUILD_CREATE"})),
            Some("GUILD_CREATE")
        );
        assert_eq!(gateway_event(&json!({"op": 11})), None);
    }

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
