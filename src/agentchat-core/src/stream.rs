//! Show an answer while it is written, then confirm it as ordinary messages.
//!
//! `AnswerStream` buffers appended text and sends the whole text so far at most
//! once per interval, so a channel can edit one message in place or append to a
//! native stream. Text past the channel's limit continues in a new message.
//! Channels without streaming get one reply when the stream finishes. Stream
//! errors propagate; nothing is retried or sent twice.
use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

use crate::{AgentChat, Channel, Error, Message, Result};

/// The opaque value a channel returns from each stream call.
pub type StreamHandle = serde_json::Value;

/// How a channel shows answers while they are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamOptions {
    /// Least time between updates.
    pub interval: Duration,
    /// Most UTF-16 code units per message; longer answers continue in a new one.
    pub limit: Option<usize>,
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(1),
            limit: None,
        }
    }
}

/// Byte index after the longest prefix of `text` that fits `units` UTF-16 code units,
/// the unit Discord, Telegram, and Teams count their limits in.
fn utf16_prefix(text: &str, units: usize) -> usize {
    let mut used = 0;
    for (index, ch) in text.char_indices() {
        used += ch.len_utf16();
        if used > units {
            return index;
        }
    }
    text.len()
}

/// Byte index where a message of at most `limit` UTF-16 units should end: after a
/// line or word break in its second half, otherwise exactly at the limit.
pub fn stream_split(text: &str, limit: usize) -> usize {
    let cut = utf16_prefix(text, limit);
    let head = &text[..cut];
    let floor = utf16_prefix(head, limit / 2);
    let late = |&i: &usize| i > floor;
    head.rfind('\n')
        .filter(late)
        .or_else(|| head.rfind(' ').filter(late))
        .map_or(cut, |i| i + 1)
}

/// An answer shown while it is written. Call [`Self::finish`] to confirm it, or
/// [`Self::abort`] when the handler fails part-way.
pub struct AnswerStream {
    app: Arc<AgentChat>,
    channel: Arc<dyn Channel>,
    source: Message,
    streaming: Option<StreamOptions>,
    handle: Option<StreamHandle>,
    messages: Vec<Message>,
    /// All appended text; `offset` bytes of it are already confirmed messages.
    text: String,
    offset: usize,
    last_flush: Option<Instant>,
    done: bool,
}

impl AnswerStream {
    pub(crate) fn new(
        app: Arc<AgentChat>,
        channel: Arc<dyn Channel>,
        source: Message,
        interval: Option<Duration>,
    ) -> Self {
        let streaming = channel.streaming().map(|options| StreamOptions {
            interval: interval.unwrap_or(options.interval),
            limit: options.limit.filter(|&limit| limit > 0),
        });
        Self {
            app,
            channel,
            source,
            streaming,
            handle: None,
            messages: Vec::new(),
            text: String::new(),
            offset: 0,
            last_flush: None,
            done: false,
        }
    }

    /// Everything appended so far.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The confirmed messages, in order; long answers span several.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    fn check_open(&self) -> Result<()> {
        if self.done {
            return Err(Error::InvalidInput(
                "This answer stream is already finished".into(),
            ));
        }
        Ok(())
    }

    /// Add text to the answer; sends an update when the interval has passed.
    pub async fn append(&mut self, text: &str) -> Result<()> {
        self.check_open()?;
        self.text.push_str(text);
        let Some(options) = self.streaming else {
            return Ok(());
        };
        if self
            .last_flush
            .is_some_and(|last| last.elapsed() < options.interval)
        {
            return Ok(());
        }
        self.last_flush = Some(Instant::now());
        self.flush(false).await
    }

    /// Send the rest and confirm the answer; returns its last message, or `None`
    /// when nothing was written.
    pub async fn finish(&mut self) -> Result<Option<Message>> {
        self.check_open()?;
        self.done = true;
        if self.streaming.is_some() {
            self.flush(true).await?;
        } else if !self.text.trim().is_empty() {
            let message = self
                .app
                .reply(self.channel.clone(), &self.source, &self.text)
                .await?;
            self.messages.push(message);
        }
        Ok(self.messages.last().cloned())
    }

    /// Close a started stream with the text so far without confirming it, for a
    /// handler that failed part-way. The handler's own error should win.
    pub async fn abort(&mut self) -> Result<()> {
        if std::mem::replace(&mut self.done, true) {
            return Ok(());
        }
        if let Some(handle) = self.handle.take() {
            let rest = &self.text[self.offset..];
            self.channel.stop_stream(&self.source, handle, rest).await?;
        }
        Ok(())
    }

    /// Send the unconfirmed text: finish full messages, then start or update the last.
    async fn flush(&mut self, finishing: bool) -> Result<()> {
        let limit = self.streaming.and_then(|options| options.limit);
        loop {
            let part = self.text[self.offset..].to_owned();
            let full = limit.is_some_and(|limit| part.encode_utf16().count() > limit);
            if part.is_empty() && !finishing {
                return Ok(());
            }
            if !full {
                if !finishing {
                    let handle = match self.handle.clone() {
                        None => self.channel.start_stream(&self.source, &part).await?,
                        Some(handle) => {
                            self.channel
                                .append_stream(&self.source, handle, &part)
                                .await?
                        }
                    };
                    self.handle = Some(handle);
                    return Ok(());
                }
                if self.handle.is_none() && part.is_empty() {
                    return Ok(());
                }
                return self.close_message(&part).await;
            }
            // Close this message at a natural break and continue in the next one.
            let mut cut = stream_split(&part, limit.unwrap_or(usize::MAX));
            if cut == 0 {
                cut = part.chars().next().map_or(part.len(), char::len_utf8);
            }
            self.close_message(&part[..cut]).await?;
        }
    }

    async fn close_message(&mut self, text: &str) -> Result<()> {
        let handle = match self.handle.clone() {
            Some(handle) => handle,
            None => self.channel.start_stream(&self.source, text).await?,
        };
        self.handle = Some(handle.clone());
        let message = self.channel.stop_stream(&self.source, handle, text).await?;
        self.handle = None;
        self.app.state().append(message.clone()).await?;
        self.offset += text.len();
        self.messages.push(message);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::stream_split as split_at;

    #[test]
    fn splits_at_a_late_break_or_the_limit() {
        assert_eq!(split_at("hello world again", 12), 12);
        assert_eq!(split_at("aaaa bbbb cccc", 12), 10);
        assert_eq!(split_at("one\ntwo three", 10), 8);
        assert_eq!(split_at("one two\nthree four", 12), 8);
        assert_eq!(split_at("abcdefghij", 4), 4);
        assert_eq!(split_at("ab cdefghij", 8), 8);
        assert_eq!(split_at("ééé ééé", 5), 7);
        // An emoji is two UTF-16 units, so only two fit in a limit of five.
        assert_eq!(split_at("😀😀😀", 5), 8);
    }
}
