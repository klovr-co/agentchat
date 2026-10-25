//! Questions with buttons: what a prompt may hold and how a button names its answer.
//!
//! Every provider puts the same short token on a button and recognizes it when the
//! button is pressed, so answers are matched to prompts without provider rules here.
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{Error, Message, Result, Sender};

/// Metadata key on an inbound message that is a button press, not a new request.
pub const ANSWER_METADATA: &str = "agentchat_answer";
/// Most buttons one prompt may offer; WhatsApp lists allow ten.
pub const MAX_CHOICES: usize = 10;
/// Longest prompt text; WhatsApp interactive bodies allow 1,024 characters.
pub const MAX_PROMPT_TEXT: usize = 1024;
/// Longest button label; Slack buttons allow 75 characters.
pub const MAX_LABEL: usize = 75;
const TOKEN_PREFIX: &str = "ac:";

fn identifier(value: &str, longest: usize) -> bool {
    !value.is_empty()
        && value.len() <= longest
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Check a prompt before any channel shows it.
pub fn validate_prompt<'a>(
    text: &str,
    choices: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<()> {
    let length = text.chars().count();
    if text.trim().is_empty() || length > MAX_PROMPT_TEXT {
        return Err(Error::InvalidInput(format!(
            "Prompt text must be 1 to {MAX_PROMPT_TEXT} characters"
        )));
    }
    let mut seen = Vec::new();
    for (id, label) in choices {
        if !identifier(id, 32) {
            return Err(Error::InvalidInput(
                "Choice IDs must be 1 to 32 letters, digits, '_' or '-'".into(),
            ));
        }
        if label.trim().is_empty() || label.chars().count() > MAX_LABEL {
            return Err(Error::InvalidInput(format!(
                "Choice labels must be 1 to {MAX_LABEL} characters"
            )));
        }
        if seen.contains(&id) {
            return Err(Error::InvalidInput("Choice IDs must be unique".into()));
        }
        seen.push(id);
    }
    if seen.is_empty() || seen.len() > MAX_CHOICES {
        return Err(Error::InvalidInput(format!(
            "A prompt needs 1 to {MAX_CHOICES} choices"
        )));
    }
    Ok(())
}

/// The value a button carries: at most 52 bytes, inside Telegram's 64-byte limit.
pub fn prompt_token(prompt: &str, choice: &str) -> Result<String> {
    if !identifier(prompt, 16) || !identifier(choice, 32) {
        return Err(Error::InvalidInput("Invalid prompt or choice ID".into()));
    }
    Ok(format!("{TOKEN_PREFIX}{prompt}:{choice}"))
}

/// The prompt and choice a pressed button names, if the value is a prompt token.
pub fn parse_prompt_token(value: &str) -> Option<(&str, &str)> {
    let (prompt, choice) = value.strip_prefix(TOKEN_PREFIX)?.split_once(':')?;
    (identifier(prompt, 16) && identifier(choice, 32)).then_some((prompt, choice))
}

/// The metadata providers attach to a button press built from a token.
pub fn answer_metadata(value: &str) -> Option<Value> {
    let (prompt, choice) = parse_prompt_token(value)?;
    Some(json!({"prompt": prompt, "choice": choice}))
}

/// A prompt's text once answered: the choice and who made it, under the question.
pub fn answered_text(text: &str, label: &str, who: &str) -> String {
    format!("{text}\n\n✓ {label} — {who}")
}

/// How a button looks; channels without styles show every button alike.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChoiceStyle {
    #[default]
    Default,
    Primary,
    Danger,
}

impl ChoiceStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Primary => "primary",
            Self::Danger => "danger",
        }
    }
}

/// One button of a prompt. `id` is returned in the answer; people see `label`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub style: ChoiceStyle,
}

impl Choice {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            style: ChoiceStyle::Default,
        }
    }

    pub fn styled(mut self, style: ChoiceStyle) -> Self {
        self.style = style;
        self
    }
}

/// A question with buttons, as AgentChat hands it to a channel to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompt {
    pub id: String,
    pub text: String,
    pub choices: Vec<Choice>,
}

impl Prompt {
    /// A checked prompt with a fresh ID.
    pub fn new(text: impl Into<String>, choices: Vec<Choice>) -> Result<Self> {
        let text = text.into();
        validate_prompt(
            &text,
            choices.iter().map(|c| (c.id.as_str(), c.label.as_str())),
        )?;
        Ok(Self {
            id: prompt_id(),
            text,
            choices,
        })
    }

    /// The token each choice's button carries.
    pub fn token(&self, choice: &Choice) -> Result<String> {
        prompt_token(&self.id, &choice.id)
    }
}

/// The button someone pressed, and who pressed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub prompt: String,
    pub choice: String,
    pub label: String,
    pub sender: Sender,
}

impl Answer {
    /// The prompt's text once answered, naming the choice and who made it.
    pub fn closed_text(&self, prompt: &Prompt) -> String {
        let who = self
            .sender
            .display_name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(&self.sender.id);
        answered_text(&prompt.text, &self.label, who)
    }
}

/// 16 hex characters, distinct within a process and hard to guess across processes.
fn prompt_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    if let Ok(elapsed) = std::time::UNIX_EPOCH.elapsed() {
        hasher.write_u128(elapsed.as_nanos());
    }
    format!("{:016x}", hasher.finish())
}

/// The prompt and choice an inbound message answers, if it is a button press.
pub fn answer_of(message: &Message) -> Option<(String, String)> {
    let answer = message.metadata.get(ANSWER_METADATA)?;
    Some((
        answer["prompt"].as_str()?.to_owned(),
        answer["choice"].as_str()?.to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_round_trip_and_reject_lookalikes() {
        let token = prompt_token("0123456789abcdef", "approve").unwrap();
        assert_eq!(token, "ac:0123456789abcdef:approve");
        assert!(token.len() <= 64);
        assert_eq!(
            parse_prompt_token(&token),
            Some(("0123456789abcdef", "approve"))
        );
        assert_eq!(
            answer_metadata(&token),
            Some(json!({"prompt": "0123456789abcdef", "choice": "approve"}))
        );
        for value in [
            "approve",
            "ac:",
            "ac:abc",
            "ac::x",
            "ac:abc:",
            "ac:a b:x",
            "ac:abc:x:y",
            "xac:abc:x",
        ] {
            assert_eq!(parse_prompt_token(value), None, "{value}");
        }
        assert!(prompt_token("a", "has space").is_err());
        assert!(prompt_token("0123456789abcdef0", "x").is_err());
    }

    #[test]
    fn prompts_are_checked_before_sending() {
        assert!(validate_prompt("Run it?", [("yes", "Approve"), ("no", "Deny")]).is_ok());
        assert!(validate_prompt(" ", [("yes", "Approve")]).is_err());
        assert!(validate_prompt("Run it?", []).is_err());
        assert!(validate_prompt("Run it?", [("yes", "A"), ("yes", "B")]).is_err());
        assert!(validate_prompt("Run it?", [("yes", "")]).is_err());
        assert!(validate_prompt("Run it?", [("no way", "No")]).is_err());
        let many: Vec<(String, String)> = (0..11).map(|i| (i.to_string(), i.to_string())).collect();
        assert!(
            validate_prompt("Pick", many.iter().map(|(a, b)| (a.as_str(), b.as_str()))).is_err()
        );
        assert!(validate_prompt(&"x".repeat(1025), [("yes", "Yes")]).is_err());
    }

    #[test]
    fn new_prompts_get_distinct_token_sized_ids() {
        let one = Prompt::new("Run it?", vec![Choice::new("yes", "Yes")]).unwrap();
        let two = Prompt::new("Run it?", vec![Choice::new("yes", "Yes")]).unwrap();
        assert_ne!(one.id, two.id);
        assert_eq!(one.id.len(), 16);
        assert!(one.token(&one.choices[0]).is_ok());
        assert!(Prompt::new("Run it?", vec![]).is_err());
    }
}
