use async_trait::async_trait;

use crate::{
    Answer, Attachment, DownloadedFile, Error, Message, Prompt, Result, RichReply, StreamHandle,
    StreamOptions, UploadFile, UploadedFile,
};

/// A transport returns a message only after provider confirmation.
/// Failed or ambiguous sends must not be transparently retried.
#[async_trait]
pub trait Channel: Send + Sync {
    fn name(&self) -> &str;

    async fn reply(&self, source: &Message, content: &str) -> Result<Message>;

    async fn close(&self) -> Result<()> {
        Ok(())
    }

    /// `None` means unsupported; errors from supported history must propagate.
    async fn thread_history(
        &self,
        _source: &Message,
        _limit: usize,
    ) -> Result<Option<Vec<Message>>> {
        Ok(None)
    }

    async fn reply_rich(&self, _source: &Message, _content: &RichReply) -> Result<Message> {
        Err(Error::Unsupported("rich replies"))
    }

    async fn upload_files(
        &self,
        _source: &Message,
        _files: &[UploadFile],
    ) -> Result<Vec<UploadedFile>> {
        Err(Error::Unsupported("file uploads"))
    }

    async fn download_attachment(
        &self,
        _source: &Message,
        _attachment: &Attachment,
        _max_bytes: usize,
    ) -> Result<DownloadedFile> {
        Err(Error::Unsupported("attachment downloads"))
    }

    async fn set_status(&self, _source: &Message, _status: &str) -> Result<bool> {
        Ok(false)
    }

    async fn mirror(
        &self,
        _source: &Message,
        _message: &Message,
        _origin: &str,
    ) -> Result<Message> {
        Err(Error::Unsupported("mirroring"))
    }

    /// How this channel shows answers while they are written; `None` means the
    /// finished answer is sent once as an ordinary reply.
    fn streaming(&self) -> Option<StreamOptions> {
        None
    }

    /// Begin showing `text`, the whole current message so far. The returned opaque
    /// handle is passed to the next stream call.
    async fn start_stream(&self, _source: &Message, _text: &str) -> Result<StreamHandle> {
        Err(Error::Unsupported("streaming"))
    }

    /// Show `text`, the whole current message so far; returns the handle to use next.
    async fn append_stream(
        &self,
        _source: &Message,
        _stream: StreamHandle,
        _text: &str,
    ) -> Result<StreamHandle> {
        Err(Error::Unsupported("streaming"))
    }

    /// Finish the message with `text` and return it once the provider confirms it.
    async fn stop_stream(
        &self,
        _source: &Message,
        _stream: StreamHandle,
        _text: &str,
    ) -> Result<Message> {
        Err(Error::Unsupported("streaming"))
    }

    /// Whether this channel can show prompts with buttons.
    fn supports_prompts(&self) -> bool {
        false
    }

    /// Show `prompt` in the source conversation with one button per choice. Each
    /// button carries `prompt.token(choice)`; a press must arrive as a message in the
    /// same conversation from the person who pressed, with `ANSWER_METADATA` set.
    async fn send_prompt(&self, _source: &Message, _prompt: &Prompt) -> Result<Message> {
        Err(Error::Unsupported("prompts"))
    }

    /// Remove a prompt's buttons once answered or expired (`None`). Channels that
    /// cannot edit a sent message may do nothing.
    async fn close_prompt(
        &self,
        _source: &Message,
        _sent: &Message,
        _prompt: &Prompt,
        _answer: Option<&Answer>,
    ) -> Result<()> {
        Ok(())
    }
}
