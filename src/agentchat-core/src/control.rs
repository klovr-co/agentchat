//! What happens to a conversation's messages while its handler is still running,
//! and the prompts that can stop or steer it.
//!
//! Each running handler has a `TurnControl`. A message that arrives meanwhile is
//! handled by the application's busy policy: steered into the running handler,
//! queued behind it, offered as a choice, or ignored. A steered message the handler
//! never reads runs afterwards as its own turn, so steering never loses a message.
//!
//! A prompt is answered once, only from its own conversation, only with one of its
//! choices, and only by someone allowed to answer it: by default the person whose
//! message it is about. Anything else is ignored.
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::{oneshot, watch};

use crate::{Answer, Channel, ConversationId, Error, Message, Prompt, Result};

/// How a message is handled when its conversation's handler is still running.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Busy {
    /// Hand the message to the running handler through `steered()`.
    #[default]
    Steer,
    /// Wait for the running handler, then run as the next turn.
    Queue,
    /// Ask the message's sender with buttons whether to queue or steer it.
    /// Channels without buttons steer.
    Ask,
    /// Record nothing and run nothing.
    Reject,
}

impl Busy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Steer => "steer",
            Self::Queue => "queue",
            Self::Ask => "ask",
            Self::Reject => "reject",
        }
    }
}

impl std::str::FromStr for Busy {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        match value {
            "steer" => Ok(Self::Steer),
            "queue" => Ok(Self::Queue),
            "ask" => Ok(Self::Ask),
            "reject" => Ok(Self::Reject),
            _ => Err(Error::InvalidInput(
                "busy must be 'steer', 'queue', 'ask', or 'reject'".into(),
            )),
        }
    }
}

/// Message metadata a channel sets when the person who started a turn asks to
/// stop it with the provider's own control, such as Slack's native stop button.
/// Such a message never starts a turn or enters history.
pub const STOP_METADATA: &str = "agentchat_stop";

/// Whether `message` asks to stop its conversation's running turn.
pub fn is_stop_request(message: &Message) -> bool {
    message.metadata.get(STOP_METADATA) == Some(&serde_json::Value::Bool(true))
}

/// Who may answer a prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Allowed {
    /// The sender of the message the prompt is about.
    #[default]
    Sender,
    /// Anyone in the conversation.
    Anyone,
    /// Only these sender IDs.
    Senders(Vec<String>),
}

impl Allowed {
    pub(crate) fn resolve(&self, source: &Message) -> Result<Option<Vec<String>>> {
        match self {
            Self::Sender => Ok(Some(vec![source.sender.id.clone()])),
            Self::Anyone => Ok(None),
            Self::Senders(ids) if ids.is_empty() => Err(Error::InvalidInput(
                "allowed must name at least one sender".into(),
            )),
            Self::Senders(ids) => Ok(Some(ids.clone())),
        }
    }
}

/// The stop signal and steered messages of one running handler.
#[derive(Clone)]
pub struct TurnControl {
    inner: Arc<TurnInner>,
}

struct TurnInner {
    stop: watch::Sender<bool>,
    /// Who sent the message this turn answers; only they may stop it from the
    /// provider's own stop control.
    starter: String,
    steered: Mutex<Vec<Message>>,
    /// Messages to run after this turn without showing them to its handler.
    queued: Mutex<Vec<Message>>,
    /// Prompts that end with this turn: its Stop button and Queue/Steer questions.
    prompts: Mutex<Vec<String>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

impl TurnControl {
    pub(crate) fn new(waiting: Vec<Message>, starter: String) -> Self {
        Self {
            inner: Arc::new(TurnInner {
                stop: watch::Sender::new(false),
                starter,
                steered: Mutex::new(waiting),
                queued: Mutex::new(Vec::new()),
                prompts: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Messages sent to this conversation since the handler started or last asked.
    /// Each is returned once; any left unread run as their own turns afterwards.
    pub fn steered(&self) -> Vec<Message> {
        std::mem::take(&mut *lock(&self.inner.steered))
    }

    /// True once someone asked this turn to stop.
    pub fn stopped(&self) -> bool {
        *self.inner.stop.borrow()
    }

    /// The sender ID of the message this turn answers.
    pub fn starter(&self) -> &str {
        &self.inner.starter
    }

    /// Ask this turn to stop. Stopping is cooperative: the handler decides how.
    pub fn stop(&self) {
        self.inner.stop.send_replace(true);
    }

    /// Wait until someone asks this turn to stop.
    pub async fn wait_stopped(&self) {
        self.until_stopped().await;
    }

    /// A future that does not keep the turn alive: `true` once stopped, `false` if
    /// the turn ended and every handle to it was dropped first.
    pub fn until_stopped(&self) -> impl Future<Output = bool> + Send + 'static {
        let mut stop = self.inner.stop.subscribe();
        async move { stop.wait_for(|stopped| *stopped).await.is_ok() }
    }

    pub(crate) fn steer(&self, message: Message) {
        lock(&self.inner.steered).push(message);
    }

    pub(crate) fn queue(&self, message: Message) {
        lock(&self.inner.queued).push(message);
    }

    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

/// What a prompt is for, and what an answer to it does.
enum Purpose {
    Ask(oneshot::Sender<Answer>),
    Stop(TurnControl),
    /// "Queue or steer?" for a message that arrived while this turn ran.
    Busy(Box<Message>, TurnControl),
}

pub(crate) struct PendingPrompt {
    pub(crate) prompt: Prompt,
    conversation: ConversationId,
    /// Sender IDs that may answer; `None` means anyone in the conversation.
    allowed: Option<Vec<String>>,
    pub(crate) channel: Arc<dyn Channel>,
    pub(crate) source: Message,
    pub(crate) sent: Option<Message>,
    purpose: Purpose,
}

impl PendingPrompt {
    /// The message this prompt held back, if nobody decided what to do with it.
    pub(crate) fn held(&self) -> Option<&Message> {
        match &self.purpose {
            Purpose::Busy(message, _) => Some(message.as_ref()),
            _ => None,
        }
    }
}

/// What an accepted press did.
pub(crate) struct Resolved {
    pub(crate) prompt: Prompt,
    pub(crate) channel: Arc<dyn Channel>,
    pub(crate) source: Message,
    pub(crate) sent: Option<Message>,
    pub(crate) answer: Answer,
    /// A held message that was just steered or queued; it now belongs in history.
    pub(crate) held: Option<Message>,
}

/// Running turns by conversation, and unanswered prompts by ID.
#[derive(Default)]
pub(crate) struct Control {
    running: HashMap<ConversationId, TurnControl>,
    prompts: HashMap<String, PendingPrompt>,
}

/// What to do with a message that is not a button press.
pub(crate) enum Arrival {
    Run,
    Steer,
    Ask(TurnControl),
    Reject,
}

impl Control {
    pub(crate) fn arrival(
        &self,
        busy: Busy,
        conversation: &ConversationId,
        prompts: bool,
    ) -> Arrival {
        let Some(turn) = self.running.get(conversation) else {
            return Arrival::Run;
        };
        match busy {
            Busy::Queue => Arrival::Run,
            Busy::Ask if prompts => Arrival::Ask(turn.clone()),
            Busy::Steer | Busy::Ask => Arrival::Steer,
            Busy::Reject => Arrival::Reject,
        }
    }

    pub(crate) fn running(&self, conversation: &ConversationId) -> Option<TurnControl> {
        self.running.get(conversation).cloned()
    }

    /// Steer into the conversation's running turn; false when none is running.
    pub(crate) fn steer(&self, message: &Message) -> bool {
        match self.running.get(&message.conversation_id) {
            Some(turn) => {
                turn.steer(message.clone());
                true
            }
            None => false,
        }
    }

    pub(crate) fn start(
        &mut self,
        conversation: &ConversationId,
        waiting: Vec<Message>,
        starter: &str,
    ) -> TurnControl {
        let turn = TurnControl::new(waiting, starter.to_owned());
        self.running.insert(conversation.clone(), turn.clone());
        turn
    }

    /// Unregister `turn`. Returns the messages to run next (unread steered ones, then
    /// queued ones) and the prompts that ended with it.
    pub(crate) fn finish(
        &mut self,
        conversation: &ConversationId,
        turn: &TurnControl,
    ) -> (Vec<Message>, Vec<PendingPrompt>) {
        if self
            .running
            .get(conversation)
            .is_some_and(|current| current.same(turn))
        {
            self.running.remove(conversation);
        }
        let mut next = turn.steered();
        next.append(&mut lock(&turn.inner.queued));
        let owned = std::mem::take(&mut *lock(&turn.inner.prompts))
            .into_iter()
            .filter_map(|id| self.prompts.remove(&id))
            .collect();
        (next, owned)
    }

    fn register(&mut self, pending: PendingPrompt, owner: Option<&TurnControl>) -> String {
        let id = pending.prompt.id.clone();
        if let Some(turn) = owner {
            lock(&turn.inner.prompts).push(id.clone());
        }
        self.prompts.insert(id.clone(), pending);
        id
    }

    pub(crate) fn register_ask(
        &mut self,
        channel: Arc<dyn Channel>,
        source: &Message,
        prompt: Prompt,
        allowed: Option<Vec<String>>,
    ) -> (String, oneshot::Receiver<Answer>) {
        let (sender, receiver) = oneshot::channel();
        let pending = PendingPrompt {
            conversation: source.conversation_id.clone(),
            prompt,
            allowed,
            channel,
            source: source.clone(),
            sent: None,
            purpose: Purpose::Ask(sender),
        };
        (self.register(pending, None), receiver)
    }

    pub(crate) fn register_stop(
        &mut self,
        channel: Arc<dyn Channel>,
        source: &Message,
        prompt: Prompt,
        allowed: Option<Vec<String>>,
        turn: TurnControl,
    ) -> String {
        let pending = PendingPrompt {
            conversation: source.conversation_id.clone(),
            prompt,
            allowed,
            channel,
            source: source.clone(),
            sent: None,
            purpose: Purpose::Stop(turn.clone()),
        };
        self.register(pending, Some(&turn))
    }

    pub(crate) fn register_busy(
        &mut self,
        channel: Arc<dyn Channel>,
        message: &Message,
        prompt: Prompt,
        turn: TurnControl,
    ) -> String {
        let pending = PendingPrompt {
            conversation: message.conversation_id.clone(),
            prompt,
            allowed: Some(vec![message.sender.id.clone()]),
            channel,
            source: message.clone(),
            sent: None,
            purpose: Purpose::Busy(Box::new(message.clone()), turn.clone()),
        };
        self.register(pending, Some(&turn))
    }

    /// Remember where a prompt was shown; false if it was answered or ended meanwhile.
    pub(crate) fn mark_sent(&mut self, id: &str, sent: &Message) -> bool {
        match self.prompts.get_mut(id) {
            Some(pending) => {
                pending.sent = Some(sent.clone());
                true
            }
            None => false,
        }
    }

    pub(crate) fn take(&mut self, id: &str) -> Option<PendingPrompt> {
        self.prompts.remove(id)
    }

    /// Apply a button press; `None` when it does not answer an open prompt.
    pub(crate) fn resolve(&mut self, press: &Message, id: &str, choice: &str) -> Option<Resolved> {
        let pending = self.prompts.get(id)?;
        let label = pending
            .prompt
            .choices
            .iter()
            .find(|c| c.id == choice)?
            .label
            .clone();
        let allowed = pending
            .allowed
            .as_ref()
            .is_none_or(|ids| ids.contains(&press.sender.id));
        if !allowed || press.conversation_id != pending.conversation {
            return None;
        }
        let PendingPrompt {
            prompt,
            channel,
            source,
            sent,
            purpose,
            ..
        } = self.prompts.remove(id)?;
        let answer = Answer {
            prompt: id.to_owned(),
            choice: choice.to_owned(),
            label,
            sender: press.sender.clone(),
        };
        let mut held = None;
        match purpose {
            Purpose::Ask(sender) => {
                let _ = sender.send(answer.clone());
            }
            Purpose::Stop(turn) => turn.stop(),
            Purpose::Busy(message, turn) => {
                if choice == "steer" {
                    turn.steer(message.as_ref().clone());
                } else {
                    turn.queue(message.as_ref().clone());
                }
                held = Some(*message);
            }
        }
        Some(Resolved {
            prompt,
            channel,
            source,
            sent,
            answer,
            held,
        })
    }
}
