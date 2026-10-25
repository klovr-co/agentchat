//! Shared policy and an asynchronous runtime for application-owned agents.
//!
//! The turn state machine is also driven by foreign-language hosts. It owns the
//! ordering policy while effects and user callbacks execute on their host loop.

mod activity;
mod channel;
mod context;
mod control;
mod error;
mod history;
mod models;
mod prompt;
mod runtime;
mod state;
mod stream;
mod turn;

pub use activity::WorkingStatus;
pub use channel::Channel;
pub use context::{Conversation, MessageContext};
pub use control::{Allowed, Busy, TurnControl};
pub use error::{Error, Result};
pub use history::{select_history, thread_history_indices, HistoryLimit};
pub use models::*;
pub use prompt::{
    answer_metadata, answer_of, answered_text, parse_prompt_token, prompt_token, validate_prompt,
    Answer, Choice, ChoiceStyle, Prompt, ANSWER_METADATA, MAX_CHOICES, MAX_LABEL, MAX_PROMPT_TEXT,
};
pub use runtime::{AgentChat, AskOptions, Handler, Runtime, StopOptions};
pub use state::{ConversationPermit, MemoryState, MemoryStore, State};
pub use stream::{stream_split, AnswerStream, StreamHandle, StreamOptions};
pub use turn::{Turn, TurnEffect, TurnResult};
