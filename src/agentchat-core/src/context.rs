use std::sync::Arc;

use crate::{
    AgentChat, Answer, AnswerStream, AskOptions, Attachment, Channel, Choice, ConversationId,
    DownloadedFile, HistoryLimit, Message, Result, RichReply, Sender, StopOptions, TurnControl,
    UploadFile, UploadedFile,
};

#[derive(Clone)]
pub struct Conversation {
    pub id: ConversationId,
    pub(crate) app: Arc<AgentChat>,
}

impl Conversation {
    pub async fn history(&self, limit: Option<usize>) -> Result<Vec<Message>> {
        self.app.history(&self.id, limit).await
    }
}

#[derive(Clone)]
pub struct MessageContext {
    pub message: Message,
    pub sender: Sender,
    pub conversation: Conversation,
    pub(crate) app: Arc<AgentChat>,
    pub(crate) channel: Arc<dyn Channel>,
    pub(crate) turn: Option<TurnControl>,
}

impl MessageContext {
    /// Begin an indicator; await its `close()` before closing the channel.
    pub async fn working(
        &self,
        status: &str,
        refresh_interval: std::time::Duration,
    ) -> Result<crate::WorkingStatus> {
        crate::WorkingStatus::start(
            self.channel.clone(),
            self.message.clone(),
            status.to_owned(),
            refresh_interval,
        )
        .await
    }

    pub fn new(app: Arc<AgentChat>, channel: Arc<dyn Channel>, message: Message) -> Self {
        Self {
            sender: message.sender.clone(),
            conversation: Conversation {
                id: message.conversation_id.clone(),
                app: app.clone(),
            },
            message,
            app,
            channel,
            turn: None,
        }
    }

    /// The running turn's stop signal and steered messages; `None` for a context
    /// built outside a handler.
    pub fn control(&self) -> Option<&TurnControl> {
        self.turn.as_ref()
    }

    /// Messages sent to this conversation since the handler started or last asked.
    /// Each is returned once; any left unread run as their own turns afterwards.
    pub fn steered(&self) -> Vec<Message> {
        self.turn
            .as_ref()
            .map(TurnControl::steered)
            .unwrap_or_default()
    }

    /// True once someone asked this turn to stop, for example with `AgentChat::stop`.
    pub fn stopped(&self) -> bool {
        self.turn.as_ref().is_some_and(TurnControl::stopped)
    }

    /// Wait until someone asks this turn to stop; never returns outside a handler.
    pub async fn wait_stopped(&self) {
        match &self.turn {
            Some(turn) => turn.wait_stopped().await,
            None => std::future::pending().await,
        }
    }

    /// Ask the sender (by default) to press a button; see [`AgentChat::ask`].
    pub async fn ask(
        &self,
        text: &str,
        choices: Vec<Choice>,
        options: AskOptions,
    ) -> Result<Answer> {
        self.app
            .ask(self.channel.clone(), &self.message, text, choices, options)
            .await
    }

    /// Show a Stop button for this turn; `None` on channels without buttons.
    pub async fn offer_stop(&self, options: StopOptions) -> Result<Option<Message>> {
        match &self.turn {
            Some(turn) => {
                self.app
                    .offer_stop_for(self.channel.clone(), &self.message, options, turn.clone())
                    .await
            }
            None => {
                self.app
                    .offer_stop(self.channel.clone(), &self.message, options)
                    .await
            }
        }
    }

    /// Show the answer while it is written; see [`AnswerStream`].
    pub fn stream(&self, interval: Option<std::time::Duration>) -> AnswerStream {
        self.app
            .stream(self.channel.clone(), &self.message, interval)
    }

    pub async fn history(&self, limit: usize) -> Result<Vec<Message>> {
        let limit = HistoryLimit::new(limit)?;
        if self.message.thread_id.is_some() {
            if let Some(mut previous) = self
                .channel
                .thread_history(&self.message, limit.get())
                .await?
            {
                previous.push(self.message.clone());
                return Ok(limit.select(&previous));
            }
        }
        self.conversation.history(Some(limit.get())).await
    }

    pub async fn reply(&self, content: &str) -> Result<Message> {
        self.app
            .reply(self.channel.clone(), &self.message, content)
            .await
    }

    pub async fn reply_rich(&self, content: &RichReply) -> Result<Message> {
        self.app
            .reply_rich(self.channel.clone(), &self.message, content)
            .await
    }

    pub async fn upload_files(&self, files: &[UploadFile]) -> Result<Vec<UploadedFile>> {
        self.app
            .upload_files(self.channel.clone(), &self.message, files)
            .await
    }

    pub async fn download_attachment(
        &self,
        attachment: &Attachment,
        max_bytes: usize,
    ) -> Result<DownloadedFile> {
        self.app
            .download_attachment(self.channel.clone(), &self.message, attachment, max_bytes)
            .await
    }

    pub async fn set_status(&self, status: &str) -> Result<bool> {
        self.app
            .set_status(self.channel.clone(), &self.message, status)
            .await
    }
}
