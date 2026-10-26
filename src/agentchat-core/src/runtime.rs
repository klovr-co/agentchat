use std::{
    future::Future,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use async_trait::async_trait;

use crate::control::{Arrival, Control, PendingPrompt};
use crate::{
    answer_of, is_stop_request, Allowed, Answer, AnswerStream, Attachment, Busy, Channel, Choice,
    ChoiceStyle, ConversationId, DownloadedFile, Error, MemoryState, Message, MessageContext,
    Prompt, Result, RichReply, Role, State, Turn, TurnControl, TurnEffect, TurnResult, UploadFile,
    UploadedFile,
};

/// An application handler; callbacks remain application-owned, never model-owned.
#[async_trait]
pub trait Handler: Send + Sync {
    async fn handle(&self, context: MessageContext) -> Result<Option<String>>;
}

#[async_trait]
impl<F, Fut> Handler for F
where
    F: Fn(MessageContext) -> Fut + Send + Sync,
    Fut: Future<Output = Result<Option<String>>> + Send,
{
    async fn handle(&self, context: MessageContext) -> Result<Option<String>> {
        self(context).await
    }
}

/// The runtime deliberately does not own the Tokio runtime or agent framework.
pub struct AgentChat {
    state: Arc<dyn State>,
    busy: Busy,
    control: Mutex<Control>,
}

pub type Runtime = AgentChat;

impl Default for AgentChat {
    fn default() -> Self {
        Self::new()
    }
}

/// Options for [`AgentChat::ask`].
#[derive(Debug, Clone)]
pub struct AskOptions {
    /// How long to wait for an answer before failing with [`Error::Timeout`].
    pub timeout: Duration,
    pub allowed: Allowed,
}

impl Default for AskOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(600),
            allowed: Allowed::Sender,
        }
    }
}

/// Options for [`AgentChat::offer_stop`].
#[derive(Debug, Clone)]
pub struct StopOptions {
    pub text: String,
    pub label: String,
    pub allowed: Allowed,
}

impl Default for StopOptions {
    fn default() -> Self {
        Self {
            text: "Working on it…".into(),
            label: "Stop".into(),
            allowed: Allowed::Sender,
        }
    }
}

const BUSY_TEXT: &str =
    "I'm still working on the previous request. What should I do with this message?";

/// Unregisters a turn whose driver was cancelled, so later messages do not steer
/// into a handler that no longer runs.
struct RunningGuard<'a> {
    app: &'a AgentChat,
    conversation: &'a ConversationId,
    turn: Option<TurnControl>,
}

impl RunningGuard<'_> {
    fn finish(mut self) -> (Vec<Message>, Vec<PendingPrompt>) {
        let turn = self.turn.take().expect("an unfinished turn");
        self.app.control().finish(self.conversation, &turn)
    }
}

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        if let Some(turn) = self.turn.take() {
            let (_, owned) = self.app.control().finish(self.conversation, &turn);
            close_detached(owned);
        }
    }
}

/// Best-effort removal of buttons when there is no task left to await it.
fn close_detached(prompts: Vec<PendingPrompt>) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    for pending in prompts {
        if let Some(sent) = pending.sent {
            let (channel, source, prompt) = (pending.channel, pending.source, pending.prompt);
            runtime.spawn(async move {
                let _ = channel.close_prompt(&source, &sent, &prompt, None).await;
            });
        }
    }
}

/// Removes an `ask()` prompt whose caller stopped waiting.
struct AskGuard<'a> {
    app: &'a AgentChat,
    id: Option<String>,
}

impl Drop for AskGuard<'_> {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            close_detached(self.app.control().take(&id).into_iter().collect());
        }
    }
}

impl AgentChat {
    pub fn new() -> Self {
        Self::with_state(Arc::new(MemoryState::new()))
    }

    pub fn with_state(state: Arc<dyn State>) -> Self {
        Self {
            state,
            busy: Busy::default(),
            control: Mutex::new(Control::default()),
        }
    }

    /// What a message does while its conversation's handler runs; steer by default.
    pub fn with_busy(mut self, busy: Busy) -> Self {
        self.busy = busy;
        self
    }

    pub fn busy(&self) -> Busy {
        self.busy
    }

    pub fn state(&self) -> &Arc<dyn State> {
        &self.state
    }

    fn control(&self) -> MutexGuard<'_, Control> {
        self.control
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// Handle one inbound message. A button press answers its prompt; another
    /// message runs the handler, or follows the busy policy while one is running.
    /// The handler may run more than once: steered messages it leaves unread and
    /// queued ones run afterwards as their own turns.
    pub async fn receive<F, Fut>(
        self: &Arc<Self>,
        channel: Arc<dyn Channel>,
        message: Message,
        handler: F,
    ) -> Result<()>
    where
        F: Fn(MessageContext) -> Fut,
        Fut: Future<Output = Result<Option<String>>>,
    {
        let mut turn = Turn::new();
        turn.resolve(TurnResult::Claimed(self.state.claim(&message.id).await?))?;
        if turn.effect() == &TurnEffect::Complete {
            return Ok(());
        }
        // A button press answers a prompt; it never starts or joins a turn.
        if let Some((prompt, choice)) = answer_of(&message) {
            return self.resolve(&message, &prompt, &choice).await;
        }
        // So does a provider's own stop control.
        if is_stop_request(&message) {
            self.stop_from(&message);
            return Ok(());
        }
        let arrival = self.control().arrival(
            self.busy,
            &message.conversation_id,
            channel.supports_prompts(),
        );
        let mut appended = false;
        match arrival {
            Arrival::Run => {}
            Arrival::Reject => return Ok(()),
            Arrival::Steer => {
                if self.steer(&message).await? {
                    return Ok(());
                }
                appended = true;
            }
            Arrival::Ask(running) => {
                if self.offer_busy(&channel, &message, running).await {
                    return Ok(());
                }
                if self.steer(&message).await? {
                    return Ok(());
                }
                appended = true;
            }
        }
        // Keep the permit until every turn completes, including reply append.
        let _permit = self.state.acquire(&message.conversation_id).await?;
        let conversation = message.conversation_id.clone();
        let mut current = message;
        let mut waiting = Vec::new();
        let mut first_error = None;
        loop {
            let control = self.control().start(
                &conversation,
                std::mem::take(&mut waiting),
                &current.sender.id,
            );
            let guard = RunningGuard {
                app: self,
                conversation: &conversation,
                turn: Some(control.clone()),
            };
            let result = self
                .run_turn(&channel, &current, appended, control, &handler)
                .await;
            let (unread, owned) = guard.finish();
            let mut next = unread;
            next.extend(self.close_owned(owned).await);
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
            let mut next = next.into_iter();
            let Some(message) = next.next() else { break };
            current = message;
            waiting = next.collect();
            appended = true;
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn run_turn<F, Fut>(
        self: &Arc<Self>,
        channel: &Arc<dyn Channel>,
        message: &Message,
        appended: bool,
        control: TurnControl,
        handler: &F,
    ) -> Result<()>
    where
        F: Fn(MessageContext) -> Fut,
        Fut: Future<Output = Result<Option<String>>>,
    {
        let mut turn = Turn::new();
        // The message was claimed and the conversation acquired before this turn.
        turn.resolve(TurnResult::Claimed(true))?;
        turn.resolve(TurnResult::ConversationAcquired)?;
        let mut control = Some(control);
        let mut confirmed_reply = None;
        loop {
            let outcome = match turn.effect() {
                TurnEffect::Claim => TurnResult::Claimed(true),
                TurnEffect::AcquireConversation => TurnResult::ConversationAcquired,
                TurnEffect::AppendIncoming => {
                    if !appended {
                        self.state.append(message.clone()).await?;
                    }
                    TurnResult::IncomingAppended
                }
                TurnEffect::InvokeHandler => {
                    let mut context =
                        MessageContext::new(self.clone(), channel.clone(), message.clone());
                    context.turn = control.take();
                    TurnResult::HandlerCompleted(handler(context).await?)
                }
                TurnEffect::SendReply(content) => {
                    confirmed_reply = Some(channel.reply(message, content).await?);
                    TurnResult::ReplySent
                }
                TurnEffect::AppendReply => {
                    let reply = confirmed_reply.take().ok_or_else(|| {
                        Error::InvalidTransition("reply was not confirmed".into())
                    })?;
                    self.state.append(reply).await?;
                    TurnResult::ReplyAppended
                }
                TurnEffect::Complete => return Ok(()),
            };
            turn.resolve(outcome)?;
        }
    }

    /// Add `message` to history and hand it to the running handler. Returns false
    /// when that handler finished meanwhile; the caller then runs the message.
    async fn steer(&self, message: &Message) -> Result<bool> {
        self.state.append(message.clone()).await?;
        Ok(self.control().steer(message))
    }

    /// Ask `message`'s sender whether it should wait for `turn` or steer it; false
    /// when the question could not be shown.
    async fn offer_busy(
        &self,
        channel: &Arc<dyn Channel>,
        message: &Message,
        turn: TurnControl,
    ) -> bool {
        let Ok(prompt) = Prompt::new(
            BUSY_TEXT,
            vec![
                Choice::new("steer", "Add to current request").styled(ChoiceStyle::Primary),
                Choice::new("queue", "Answer after it"),
            ],
        ) else {
            return false;
        };
        let id = self
            .control()
            .register_busy(channel.clone(), message, prompt.clone(), turn);
        self.show(&id, channel, message, &prompt).await.is_ok()
    }

    /// Show a registered prompt and remember where it went.
    async fn show(
        &self,
        id: &str,
        channel: &Arc<dyn Channel>,
        source: &Message,
        prompt: &Prompt,
    ) -> Result<Message> {
        let sent = match channel.send_prompt(source, prompt).await {
            Ok(sent) => sent,
            Err(error) => {
                self.control().take(id);
                return Err(error);
            }
        };
        if !self.control().mark_sent(id, &sent) {
            // Answered or ended while it was being sent; take the buttons away again.
            let _ = channel.close_prompt(source, &sent, prompt, None).await;
        }
        Ok(sent)
    }

    /// Apply a button press; a press that does not match an open prompt is ignored.
    async fn resolve(&self, press: &Message, prompt: &str, choice: &str) -> Result<()> {
        let Some(resolved) = self.control().resolve(press, prompt, choice) else {
            return Ok(());
        };
        if let Some(message) = resolved.held {
            self.state.append(message).await?;
        }
        if let Some(sent) = resolved.sent {
            let _ = resolved
                .channel
                .close_prompt(
                    &resolved.source,
                    &sent,
                    &resolved.prompt,
                    Some(&resolved.answer),
                )
                .await;
        }
        Ok(())
    }

    /// Close a finished turn's prompts. Returns held messages nobody decided on;
    /// they run next, as if queued.
    async fn close_owned(&self, owned: Vec<PendingPrompt>) -> Vec<Message> {
        let mut undecided = Vec::new();
        for pending in owned {
            if let Some(message) = pending.held() {
                if self.state.append(message.clone()).await.is_ok() {
                    undecided.push(message.clone());
                }
            }
            if let Some(sent) = &pending.sent {
                let _ = pending
                    .channel
                    .close_prompt(&pending.source, sent, &pending.prompt, None)
                    .await;
            }
        }
        undecided
    }

    /// Show `text` with one button per choice and wait for an allowed person to
    /// press one. Fails with [`Error::Unsupported`] on channels without buttons and
    /// [`Error::Timeout`] when nobody answers, so an unanswered approval is never a yes.
    pub async fn ask(
        &self,
        channel: Arc<dyn Channel>,
        source: &Message,
        text: &str,
        choices: Vec<Choice>,
        options: AskOptions,
    ) -> Result<Answer> {
        if !channel.supports_prompts() {
            return Err(Error::Unsupported("prompts"));
        }
        if options.timeout.is_zero() {
            return Err(Error::InvalidInput(
                "Prompt timeout must be positive".into(),
            ));
        }
        let prompt = Prompt::new(text, choices)?;
        let allowed = options.allowed.resolve(source)?;
        let (id, mut answer) =
            self.control()
                .register_ask(channel.clone(), source, prompt.clone(), allowed);
        let mut guard = AskGuard {
            app: self,
            id: Some(id.clone()),
        };
        self.show(&id, &channel, source, &prompt).await?;
        if let Ok(Ok(answer)) = tokio::time::timeout(options.timeout, &mut answer).await {
            guard.id = None;
            return Ok(answer);
        }
        guard.id = None;
        let Some(pending) = self.control().take(&id) else {
            // Answered just as the wait ended.
            return answer
                .try_recv()
                .map_err(|_| Error::Timeout("Nobody answered the prompt in time".into()));
        };
        if let Some(sent) = &pending.sent {
            let _ = channel.close_prompt(source, sent, &prompt, None).await;
        }
        Err(Error::Timeout("Nobody answered the prompt in time".into()))
    }

    /// Show a Stop button for the conversation's running handler; returns the shown
    /// message, or `None` on channels without buttons. It goes away when the turn ends.
    pub async fn offer_stop(
        &self,
        channel: Arc<dyn Channel>,
        source: &Message,
        options: StopOptions,
    ) -> Result<Option<Message>> {
        let turn = self
            .control()
            .running(&source.conversation_id)
            .ok_or_else(|| {
                Error::InvalidInput("No handler is running for this conversation".into())
            })?;
        self.offer_stop_for(channel, source, options, turn).await
    }

    /// [`Self::offer_stop`] for a specific turn, for hosts that hold its control.
    pub async fn offer_stop_for(
        &self,
        channel: Arc<dyn Channel>,
        source: &Message,
        options: StopOptions,
        turn: TurnControl,
    ) -> Result<Option<Message>> {
        if !channel.supports_prompts() {
            return Ok(None);
        }
        let prompt = Prompt::new(
            options.text,
            vec![Choice::new("stop", options.label).styled(ChoiceStyle::Danger)],
        )?;
        let allowed = options.allowed.resolve(source)?;
        let id =
            self.control()
                .register_stop(channel.clone(), source, prompt.clone(), allowed, turn);
        self.show(&id, &channel, source, &prompt).await.map(Some)
    }

    /// Ask the conversation's running handler to stop; false when none is running.
    pub fn stop(&self, conversation: &ConversationId) -> bool {
        match self.control().running(conversation) {
            Some(turn) => {
                turn.stop();
                true
            }
            None => false,
        }
    }

    /// Apply a provider stop request: it stops the running turn only when sent by
    /// the person whose message started it, like a default Stop button.
    fn stop_from(&self, request: &Message) -> bool {
        match self.control().running(&request.conversation_id) {
            Some(turn) if turn.starter() == request.sender.id => {
                turn.stop();
                true
            }
            _ => false,
        }
    }

    /// The control of the conversation's running handler, if one is running.
    pub fn running(&self, conversation: &ConversationId) -> Option<TurnControl> {
        self.control().running(conversation)
    }

    /// Show an answer in `source`'s conversation while it is written. `interval`
    /// overrides the channel's least time between updates.
    pub fn stream(
        self: &Arc<Self>,
        channel: Arc<dyn Channel>,
        source: &Message,
        interval: Option<Duration>,
    ) -> AnswerStream {
        AnswerStream::new(self.clone(), channel, source.clone(), interval)
    }

    pub async fn receive_with_handler(
        self: &Arc<Self>,
        channel: Arc<dyn Channel>,
        message: Message,
        handler: &dyn Handler,
    ) -> Result<()> {
        self.receive(channel, message, |context| handler.handle(context))
            .await
    }

    pub async fn reply(
        &self,
        channel: Arc<dyn Channel>,
        source: &Message,
        content: &str,
    ) -> Result<Message> {
        let reply = channel.reply(source, content).await?;
        self.state.append(reply.clone()).await?;
        Ok(reply)
    }

    pub async fn reply_rich(
        &self,
        channel: Arc<dyn Channel>,
        source: &Message,
        content: &RichReply,
    ) -> Result<Message> {
        let reply = channel.reply_rich(source, content).await?;
        self.state.append(reply.clone()).await?;
        Ok(reply)
    }

    pub async fn upload_files(
        &self,
        channel: Arc<dyn Channel>,
        source: &Message,
        files: &[UploadFile],
    ) -> Result<Vec<UploadedFile>> {
        channel.upload_files(source, files).await
    }

    pub async fn download_attachment(
        &self,
        channel: Arc<dyn Channel>,
        source: &Message,
        attachment: &Attachment,
        max_bytes: usize,
    ) -> Result<DownloadedFile> {
        if !source.attachments.contains(attachment) {
            return Err(Error::InvalidInput(
                "Attachment does not belong to the source message".into(),
            ));
        }
        channel
            .download_attachment(source, attachment, max_bytes)
            .await
    }

    pub async fn set_status(
        &self,
        channel: Arc<dyn Channel>,
        source: &Message,
        status: &str,
    ) -> Result<bool> {
        channel.set_status(source, status).await
    }

    /// Post a notice only `user_id` can see in `source`'s conversation. It is not
    /// added to history. Fails with [`Error::Unsupported`] on channels without
    /// private notices; it is never sent publicly instead.
    pub async fn send_ephemeral(
        &self,
        channel: Arc<dyn Channel>,
        source: &Message,
        user_id: &str,
        content: &RichReply,
    ) -> Result<Message> {
        if user_id.trim().is_empty() {
            return Err(Error::InvalidInput(
                "An ephemeral message needs a recipient".into(),
            ));
        }
        channel.send_ephemeral(source, user_id, content).await
    }

    /// Delete a message this application sent. History keeps it.
    pub async fn delete(&self, channel: Arc<dyn Channel>, message: &Message) -> Result<()> {
        channel.delete(message).await
    }

    pub async fn mirror(
        &self,
        channel: Arc<dyn Channel>,
        source: &Message,
        message: &Message,
        origin: &str,
    ) -> Result<Message> {
        if message.role != Role::User {
            return Err(Error::InvalidInput(
                "Only user inputs can be mirrored".into(),
            ));
        }
        let mirrored = channel.mirror(source, message, origin).await?;
        self.state.append(mirrored.clone()).await?;
        Ok(mirrored)
    }

    pub async fn history(
        &self,
        conversation: &ConversationId,
        limit: Option<usize>,
    ) -> Result<Vec<Message>> {
        self.state.history(conversation, limit).await
    }
}
