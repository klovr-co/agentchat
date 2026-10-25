//! BlueBubbles addressing, event policy, and credential-safe endpoint configuration.
use crate::wire::{quote, strip, truthy};
use agentchat_core::{Attachment, Message, Role, Sender};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
#[derive(Clone)]
pub struct Server {
    pub url: String,
}
impl Server {
    pub fn parse(raw: &str) -> Result<Self, &'static str> {
        let url = url::Url::parse(raw)
            .map_err(|_| "BlueBubbles requires a root server URL without credentials or query")?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.path(), "" | "/")
        {
            return Err("BlueBubbles requires a root server URL without credentials or query");
        }
        if url.scheme() == "http"
            && !matches!(
                url.host_str(),
                Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
            )
        {
            return Err("Use HTTPS for a remote BlueBubbles server");
        }
        Ok(Self {
            url: url.as_str().trim_end_matches('/').into(),
        })
    }
    pub fn instance_id(&self) -> String {
        format!("{:x}", Sha256::digest(self.url.as_bytes()))[..24].into()
    }
    pub fn socket_url(&self, password: &str) -> String {
        format!(
            "{}?password={}",
            self.url,
            quote(&password.replace('%', "%25"))
        )
    }
}
pub fn text_payload(content: &str) -> Result<&str, &'static str> {
    if strip(content).is_empty() {
        Err("iMessage replies require nonempty text")
    } else {
        Ok(content)
    }
}
pub fn normalize(event: &Value, instance_id: &str) -> Option<Message> {
    let e = event.as_object()?;
    if e.get("isFromMe") != Some(&Value::Bool(false)) {
        return None;
    }
    for flag in [
        "isSystemMessage",
        "isServiceMessage",
        "isAutoReply",
        "isSpam",
    ] {
        if e.get(flag).is_some_and(truthy) {
            return None;
        }
    }
    for field in ["associatedMessageType", "itemType"] {
        if e.get(field)
            .is_some_and(|v| !v.is_null() && v != &json!(0) && v != &Value::Bool(false))
        {
            return None;
        }
    }
    let chats = e.get("chats")?.as_array()?;
    if chats.len() != 1 {
        return None;
    }
    let chat = chats[0].get("guid")?.as_str()?;
    let guid = e.get("guid")?.as_str()?;
    let address = e.get("handle")?.get("address")?.as_str()?;
    if chat.is_empty() || guid.is_empty() || address.is_empty() || !chat.starts_with("iMessage;") {
        return None;
    }
    let text = match e.get("text") {
        Some(v) if truthy(v) => v.as_str()?,
        _ => "",
    };
    let text = strip(&text.replace('\u{fffc}', "")).to_owned();
    let attachments = e
        .get("attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let id = a.get("guid")?.as_str()?.to_owned();
            if id.is_empty() {
                return None;
            }
            Some(Attachment {
                id,
                name: a
                    .get("transferName")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                media_type: a.get("mimeType").and_then(Value::as_str).map(str::to_owned),
                size: a.get("totalBytes").and_then(Value::as_u64),
            })
        })
        .collect::<Vec<_>>();
    if text.is_empty() && attachments.is_empty() {
        return None;
    }
    Some(Message {
        id: format!("imessage:{}:{guid}", quote(instance_id)).into(),
        conversation_id: format!("imessage:{}:{}", quote(instance_id), quote(chat)).into(),
        channel: "imessage".into(),
        sender: Sender {
            id: address.into(),
            display_name: None,
        },
        text,
        role: Role::User,
        metadata: json!({"instance_id":instance_id,"chat_guid":chat,"message_guid":guid})
            .as_object()?
            .clone(),
        thread_id: None,
        addressed: true,
        attachments,
    })
}
/// Whether a chat is one-to-one (`iMessage;-;…`) rather than a group (`iMessage;+;…`).
pub fn is_direct(chat_guid: &str) -> bool {
    chat_guid.starts_with("iMessage;-;")
}

/// Whether `text` is exactly the configured stop keyword, ignoring case and spaces.
pub fn is_stop_keyword(text: &str, keyword: Option<&str>) -> bool {
    keyword
        .map(str::trim)
        .filter(|keyword| !keyword.is_empty())
        .is_some_and(|keyword| text.trim().eq_ignore_ascii_case(keyword))
}

/// The message as a stop request for its conversation, from its sender.
pub fn stop_request(message: &Message) -> Message {
    let mut stop = message.clone();
    stop.id = format!("{}:stop", message.id).into();
    stop.text = String::new();
    stop.attachments.clear();
    stop.metadata
        .insert(agentchat_core::STOP_METADATA.into(), json!(true));
    stop
}

#[derive(Debug, Clone)]
pub struct Destination {
    pub instance_id: String,
    pub chat_guid: String,
}
impl Destination {
    pub fn from_source(
        source: &Message,
        instance: &str,
        allowed: Option<&[String]>,
    ) -> Result<Self, &'static str> {
        let chat = source
            .metadata
            .get("chat_guid")
            .and_then(Value::as_str)
            .filter(|s| s.starts_with("iMessage;"));
        if source.channel != "imessage"
            || source.metadata.get("instance_id").and_then(Value::as_str) != Some(instance)
            || chat.is_none()
        {
            return Err("Source does not belong to this iMessage adapter");
        }
        let chat = chat.unwrap();
        if allowed.is_some_and(|values| !values.iter().any(|v| v == chat)) {
            return Err("iMessage conversation is not allowed");
        }
        Ok(Self {
            instance_id: instance.into(),
            chat_guid: chat.into(),
        })
    }
    pub fn confirmed_reply(
        &self,
        source: &Message,
        content: String,
        receipt: &str,
    ) -> Result<Message, &'static str> {
        if receipt.is_empty() {
            return Err("BlueBubbles did not return a message receipt");
        }
        let mut result = source.clone();
        result.id = format!("imessage:{}:{receipt}", quote(&self.instance_id)).into();
        result.sender = Sender {
            id: format!("imessage:{}:me", self.instance_id),
            display_name: None,
        };
        result.text = content;
        result.role = Role::Assistant;
        result.addressed = true;
        result.thread_id = None;
        result.attachments.clear();
        result
            .metadata
            .insert("message_guid".into(), receipt.into());
        Ok(result)
    }
}
pub fn text_request(chat: &str, content: &str, temporary_id: &str) -> Value {
    json!({"chatGuid":chat,"message":content,"method":"apple-script","tempGuid":temporary_id})
}
/// Seconds between typing refreshes; the server stops typing after about ten.
pub const TYPING_INTERVAL: f64 = 5.0;
/// Stop refreshing a forgotten indicator after this many seconds.
pub const TYPING_EXPIRY: f64 = 120.0;

/// `POST` here shows "typing…" in the chat. It needs the server's Private API.
/// `DELETE` on the same route restarts typing on current servers, so it is not used.
pub fn typing_path(chat: &str) -> String {
    format!("/api/v1/chat/{}/typing", quote(chat))
}

/// The socket event that clears "typing…" for `typing_event_payload(chat)`.
pub const STOP_TYPING_EVENT: &str = "stopped-typing";

pub fn typing_event_payload(chat: &str) -> Value {
    json!({"chatGuid": chat})
}

/// Whether a failed typing request means the server cannot show typing at all:
/// the Private API is off or its helper is not connected (an "iMessage Error"),
/// or the server predates the route.
pub fn typing_unavailable(status: u16, response: &Value) -> bool {
    status == 404 || (status == 500 && response["error"]["type"] == "iMessage Error")
}

pub fn receipt(response: &Value) -> Result<&str, &'static str> {
    let data = &response["data"];
    if data
        .get("error")
        .is_some_and(|v| !v.is_null() && v != &json!(0) && v != &Value::Bool(false))
    {
        return Err("BlueBubbles did not return a successful message receipt");
    }
    data.get("guid")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or("BlueBubbles did not return a successful message receipt")
}
#[cfg(test)]
mod app_tests {
    use super::*;

    #[test]
    fn direct_chats_and_stop_keywords() {
        assert!(is_direct("iMessage;-;+1555") && !is_direct("iMessage;+;chat1"));
        assert!(is_stop_keyword(" STOP ", Some("stop")));
        assert!(!is_stop_keyword("stop now", Some("stop")) && !is_stop_keyword("stop", None));
        let event = json!({"isFromMe": false, "guid": "g", "handle": {"address": "alice"},
            "chats": [{"guid": "iMessage;-;alice"}], "text": "stop"});
        let stop = stop_request(&normalize(&event, "mac").unwrap());
        assert!(agentchat_core::is_stop_request(&stop));
        assert_eq!(stop.sender.id, "alice");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn url_and_password() {
        let server = Server::parse("http://localhost:123/").unwrap();
        assert_eq!(
            server.socket_url("a%& b"),
            "http://localhost:123?password=a%2525%26%20b"
        );
        assert!(Server::parse("http://remote.test").is_err());
        assert!(Server::parse("https://x.test/path").is_err());
    }
    #[test]
    fn typing_needs_the_private_api() {
        assert_eq!(
            typing_path("iMessage;-;+1555"),
            "/api/v1/chat/iMessage%3B-%3B%2B1555/typing"
        );
        let disabled = json!({"status": 500, "error": {"type": "iMessage Error",
            "message": "iMessage Private API is not enabled!"}});
        assert!(typing_unavailable(500, &disabled));
        assert!(typing_unavailable(404, &Value::Null));
        assert!(!typing_unavailable(
            500,
            &json!({"error": {"type": "Server Error"}})
        ));
        assert!(!typing_unavailable(401, &Value::Null));
    }
    #[test]
    fn encoding() {
        assert_eq!(quote("a:b/雪~"), "a%3Ab%2F%E9%9B%AA~");
        assert_eq!(strip("\u{1c} x \u{1f}"), "x");
    }

    #[test]
    fn attachment_only_message_and_allowlisted_reply_share_identity() {
        let event = json!({"isFromMe":false,"guid":"incoming","handle":{"address":"alice"},"chats":[{"guid":"iMessage;+;雪"}],"text":"\u{fffc}","attachments":[{"guid":"file","transferName":"雪.png","totalBytes":50}]});
        let message = normalize(&event, "mac:1").unwrap();
        assert_eq!(message.text, "");
        assert_eq!(message.attachments[0].size, Some(50));
        assert_eq!(
            message.conversation_id.as_ref(),
            "imessage:mac%3A1:iMessage%3B%2B%3B%E9%9B%AA"
        );
        assert!(Destination::from_source(&message, "mac:1", Some(&[])).is_err());
        let route =
            Destination::from_source(&message, "mac:1", Some(&["iMessage;+;雪".into()])).unwrap();
        let response = route
            .confirmed_reply(&message, "hello".into(), "confirmed")
            .unwrap();
        assert_eq!(response.id.as_ref(), "imessage:mac%3A1:confirmed");
        assert_eq!(response.conversation_id, message.conversation_id);
        assert!(response.attachments.is_empty());
        let mut reaction = event;
        reaction["associatedMessageType"] = 2000.into();
        assert!(normalize(&reaction, "mac:1").is_none());
    }
}
