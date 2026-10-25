//! Provider payloads that are not chat messages, routed to the application's own
//! handlers: events, button presses, form submissions and commands.
use serde::{Deserialize, Serialize};

/// Where a provider payload that AgentChat does not handle itself is routed. Each
/// provider decides the route; applications register handlers by kind and key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Route {
    /// A provider event that did not become a message, by its type.
    Event { event_type: String },
    /// A press on a button or menu that is not an AgentChat prompt answer.
    Action { action_ids: Vec<String> },
    /// A form or modal submission (or its dismissal), by its callback ID.
    View { callback_id: String },
    /// A command, such as `/deploy`.
    Command { command: String },
    /// Anything else.
    Other,
}

/// The command a text message starts with, such as `/deploy` for
/// `"/deploy@my_bot staging"`: a leading `/`, then letters, digits, `_` or `-`.
/// A `@bot` suffix is dropped. `None` for any other text.
pub fn command_of(text: &str) -> Option<String> {
    let word = text.trim_start().split_whitespace().next()?;
    let name = word.strip_prefix('/')?;
    let name = name.split_once('@').map_or(name, |(name, _)| name);
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    Some(format!("/{name}"))
}

#[cfg(test)]
mod tests {
    use super::command_of;

    #[test]
    fn commands_are_a_leading_slash_word() {
        assert_eq!(command_of("/deploy staging").as_deref(), Some("/deploy"));
        assert_eq!(command_of("  /start@my_bot").as_deref(), Some("/start"));
        assert_eq!(command_of("/a-b_c").as_deref(), Some("/a-b_c"));
        for text in ["deploy", "/", "/ x", "a /deploy", "//x", "/x.y", "/x/y"] {
            assert_eq!(command_of(text), None, "{text}");
        }
    }
}
