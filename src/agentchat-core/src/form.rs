//! Forms asked in chat, one field at a time, for channels without modals.
//!
//! A `Form` names its fields once. A `FormSession` asks them in order: choices
//! and yes/no questions offer buttons where the channel has them (and numbered
//! options otherwise); text fields take the person's next message. The values
//! go to the application's `on_view(callback_id)` handler like a modal
//! submission. "cancel" ends the form; "skip" leaves an optional field empty.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{Choice, Error, Result, MAX_CHOICES, MAX_LABEL, MAX_PROMPT_TEXT};

/// Most fields one form may ask.
pub const MAX_FIELDS: usize = 20;
/// Seconds a person has to answer each question unless the form sets `timeout`.
pub const DEFAULT_TIMEOUT: f64 = 600.0;
/// The reply that ends a form without submitting it.
pub const CANCEL_WORD: &str = "cancel";
/// The reply that leaves an optional field empty.
pub const SKIP_WORD: &str = "skip";

fn default_timeout() -> f64 {
    DEFAULT_TIMEOUT
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    /// Any text; the value is a string.
    #[default]
    Text,
    /// One of `options`; the value is the option's `value`.
    Choice,
    /// Yes or no; the value is a boolean.
    YesNo,
}

/// One option of a choice field: a plain string, or `{"value": ..., "label": ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "OptionInput")]
pub struct FormOption {
    pub value: String,
    pub label: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OptionInput {
    Plain(String),
    Full {
        value: String,
        #[serde(default)]
        label: Option<String>,
    },
}

impl From<OptionInput> for FormOption {
    fn from(input: OptionInput) -> Self {
        match input {
            OptionInput::Plain(value) => Self {
                label: value.clone(),
                value,
            },
            OptionInput::Full { value, label } => Self {
                label: label.unwrap_or_else(|| value.clone()),
                value,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormField {
    pub id: String,
    pub label: String,
    #[serde(default, rename = "type")]
    pub kind: FieldKind,
    #[serde(default)]
    pub options: Vec<FormOption>,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Form {
    /// Names the form's `on_view` handler, like a modal's callback ID.
    pub callback_id: String,
    /// Shown before the first question.
    #[serde(default)]
    pub title: Option<String>,
    pub fields: Vec<FormField>,
    /// Seconds the person has to answer each question.
    #[serde(default = "default_timeout")]
    pub timeout: f64,
}

impl Form {
    pub fn validate(&self) -> Result<()> {
        let invalid = |text: &str| Err(Error::InvalidInput(text.into()));
        if self.callback_id.trim().is_empty() {
            return invalid("A form needs a callback_id");
        }
        if self.fields.is_empty() || self.fields.len() > MAX_FIELDS {
            return Err(Error::InvalidInput(format!(
                "A form needs 1 to {MAX_FIELDS} fields"
            )));
        }
        if !(self.timeout.is_finite() && self.timeout > 0.0) {
            return invalid("A form's timeout must be a positive number of seconds");
        }
        let mut seen = Vec::new();
        for field in &self.fields {
            if field.id.trim().is_empty() || seen.contains(&field.id.as_str()) {
                return invalid("Form fields need unique, nonempty IDs");
            }
            seen.push(field.id.as_str());
            if field.label.trim().is_empty() || field.label.chars().count() > MAX_PROMPT_TEXT / 2 {
                return Err(Error::InvalidInput(format!(
                    "Form field labels must be 1 to {} characters",
                    MAX_PROMPT_TEXT / 2
                )));
            }
            match field.kind {
                FieldKind::Choice => {
                    if field.options.is_empty() {
                        return invalid("A choice field needs options");
                    }
                    let mut values = Vec::new();
                    for option in &field.options {
                        if option.value.is_empty()
                            || option.label.trim().is_empty()
                            || values.contains(&option.value.as_str())
                        {
                            return invalid("Choice options need unique values and labels");
                        }
                        values.push(option.value.as_str());
                    }
                }
                FieldKind::Text | FieldKind::YesNo if !field.options.is_empty() => {
                    return invalid("Only choice fields have options");
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// One question to show: its text, and the buttons to offer when the channel has
/// them (choice IDs and labels). Without buttons, show `numbered()` instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub text: String,
    pub choices: Vec<Choice>,
}

impl Question {
    /// Whether every choice fits a button.
    pub fn buttons(&self) -> bool {
        !self.choices.is_empty()
            && self.choices.len() <= MAX_CHOICES
            && self
                .choices
                .iter()
                .all(|c| c.label.chars().count() <= MAX_LABEL)
            && self.text.chars().count() <= MAX_PROMPT_TEXT
    }

    /// The question with its choices as a numbered list, for answering by text.
    pub fn numbered(&self) -> String {
        let mut text = self.text.clone();
        for (index, choice) in self.choices.iter().enumerate() {
            text.push_str(&format!("\n{}. {}", index + 1, choice.label));
        }
        text
    }
}

/// What to do after an answer.
#[derive(Debug, Clone, PartialEq)]
pub enum FormStep {
    /// Ask this question next (the next field, or the same one again after an
    /// answer that did not fit, with the reason first).
    Ask(Question),
    /// Every field is answered: submit these values.
    Submit(Map<String, Value>),
    /// The person cancelled.
    Cancel,
}

/// A form being asked of one person.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormSession {
    pub form: Form,
    /// The field being asked.
    pub index: usize,
    pub values: Map<String, Value>,
    /// The prompt whose buttons answer the current question, when shown with any.
    #[serde(default)]
    pub prompt: Option<String>,
}

impl FormSession {
    /// A checked form's session and its first question.
    pub fn start(form: Form) -> Result<(Self, Question)> {
        form.validate()?;
        let session = Self {
            form,
            index: 0,
            values: Map::new(),
            prompt: None,
        };
        let question = session.question();
        Ok((session, question))
    }

    /// The current question.
    pub fn question(&self) -> Question {
        let field = &self.form.fields[self.index];
        let mut text = String::new();
        if self.index == 0 {
            if let Some(title) = self.form.title.as_deref().filter(|t| !t.trim().is_empty()) {
                text.push_str(&format!("**{}**\n", title.trim()));
            }
        }
        text.push_str(field.label.trim());
        let mut hints = Vec::new();
        if field.optional {
            hints.push(format!("\"{SKIP_WORD}\" to leave it empty"));
        }
        if self.index == 0 {
            hints.push(format!("\"{CANCEL_WORD}\" to stop"));
        }
        if !hints.is_empty() {
            text.push_str(&format!("\n(Reply {})", hints.join(", or ")));
        }
        let choices = match field.kind {
            FieldKind::Text => Vec::new(),
            FieldKind::Choice => field
                .options
                .iter()
                .enumerate()
                .map(|(index, option)| Choice::new(format!("o{index}"), option.label.clone()))
                .collect(),
            FieldKind::YesNo => vec![Choice::new("yes", "Yes"), Choice::new("no", "No")],
        };
        Question { text, choices }
    }

    /// Take a text reply.
    pub fn answer_text(&mut self, text: &str) -> FormStep {
        let reply = text.trim();
        if reply.eq_ignore_ascii_case(CANCEL_WORD) {
            return FormStep::Cancel;
        }
        let field = &self.form.fields[self.index];
        if field.optional && reply.eq_ignore_ascii_case(SKIP_WORD) {
            return self.accept(Value::Null);
        }
        let value = match field.kind {
            FieldKind::Text if !reply.is_empty() => Some(Value::String(reply.into())),
            FieldKind::Text => None,
            // "1" and "2" follow the numbered options.
            FieldKind::YesNo => match reply.to_lowercase().as_str() {
                "yes" | "y" | "1" => Some(Value::Bool(true)),
                "no" | "n" | "2" => Some(Value::Bool(false)),
                _ => None,
            },
            FieldKind::Choice => {
                let by_number = reply
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .and_then(|n| field.options.get(n));
                by_number
                    .or_else(|| {
                        field.options.iter().find(|o| {
                            o.label.eq_ignore_ascii_case(reply)
                                || o.value.eq_ignore_ascii_case(reply)
                        })
                    })
                    .map(|o| Value::String(o.value.clone()))
            }
        };
        match value {
            Some(value) => self.accept(value),
            None => FormStep::Ask(self.retry()),
        }
    }

    /// Take a button press, if it answers the current question.
    pub fn answer_choice(&mut self, prompt: &str, choice: &str) -> Option<FormStep> {
        if self.prompt.as_deref() != Some(prompt) {
            return None;
        }
        let field = &self.form.fields[self.index];
        let value = match field.kind {
            FieldKind::YesNo => Value::Bool(choice == "yes"),
            FieldKind::Choice => {
                let index: usize = choice.strip_prefix('o')?.parse().ok()?;
                Value::String(field.options.get(index)?.value.clone())
            }
            FieldKind::Text => return None,
        };
        Some(self.accept(value))
    }

    fn accept(&mut self, value: Value) -> FormStep {
        let field = &self.form.fields[self.index];
        self.values.insert(field.id.clone(), value);
        self.prompt = None;
        self.index += 1;
        if self.index == self.form.fields.len() {
            FormStep::Submit(self.values.clone())
        } else {
            FormStep::Ask(self.question())
        }
    }

    fn retry(&self) -> Question {
        let field = &self.form.fields[self.index];
        let mut question = self.question();
        let reason = match field.kind {
            FieldKind::Text => "Please reply with some text.",
            FieldKind::YesNo => "Please answer yes or no.",
            FieldKind::Choice => "Please pick one of the options.",
        };
        question.text = format!("{reason}\n{}", question.text);
        question
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn form() -> Form {
        serde_json::from_value(json!({
            "callback_id": "signup",
            "title": "Sign up",
            "fields": [
                {"id": "name", "label": "Your name?"},
                {"id": "plan", "label": "Which plan?", "type": "choice",
                 "options": ["Free", {"value": "pro", "label": "Pro"}]},
                {"id": "news", "label": "Newsletter?", "type": "yes_no", "optional": true},
            ],
        }))
        .unwrap()
    }

    #[test]
    fn asks_each_field_and_submits_the_values() {
        let (mut session, first) = FormSession::start(form()).unwrap();
        assert_eq!(
            first.text,
            "**Sign up**\nYour name?\n(Reply \"cancel\" to stop)"
        );
        assert!(first.choices.is_empty() && !first.buttons());
        let FormStep::Ask(plan) = session.answer_text("  Ada ") else {
            panic!("expected the next question")
        };
        assert_eq!(plan.numbered(), "Which plan?\n1. Free\n2. Pro");
        assert!(plan.buttons());
        let FormStep::Ask(retry) = session.answer_text("Gold") else {
            panic!("expected the question again")
        };
        assert!(retry.text.starts_with("Please pick one of the options."));
        session.prompt = Some("p1".into());
        assert_eq!(session.answer_choice("p0", "o1"), None);
        let Some(FormStep::Ask(news)) = session.answer_choice("p1", "o1") else {
            panic!("expected the last question")
        };
        assert!(news.text.contains("\"skip\""));
        let FormStep::Submit(values) = session.answer_text("skip") else {
            panic!("expected the values")
        };
        assert_eq!(
            Value::Object(values),
            json!({"name": "Ada", "plan": "pro", "news": null})
        );
    }

    #[test]
    fn numbers_labels_yes_no_and_cancel() {
        let (mut session, _) = FormSession::start(form()).unwrap();
        session.answer_text("Ada");
        assert!(matches!(session.answer_text("1"), FormStep::Ask(_)));
        assert_eq!(session.values["plan"], "Free");
        let FormStep::Submit(values) = session.answer_text("Y") else {
            panic!("expected the values")
        };
        assert_eq!(values["news"], true);
        let (mut session, _) = FormSession::start(form()).unwrap();
        assert_eq!(session.answer_text(" Cancel "), FormStep::Cancel);
    }

    #[test]
    fn rejects_forms_that_cannot_be_asked() {
        let mut bad = form();
        bad.fields[1].options.clear();
        assert!(bad.validate().is_err());
        let mut bad = form();
        bad.fields[0].options = vec![FormOption::from(OptionInput::Plain("x".into()))];
        assert!(bad.validate().is_err());
        let mut bad = form();
        bad.fields[2].id = "name".into();
        assert!(bad.validate().is_err());
        let mut bad = form();
        bad.callback_id = " ".into();
        assert!(bad.validate().is_err());
        assert!(
            serde_json::from_value::<Form>(json!({"callback_id": "x", "fields": []}))
                .unwrap()
                .validate()
                .is_err()
        );
    }
}
