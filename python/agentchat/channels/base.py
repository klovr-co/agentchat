from __future__ import annotations

from collections.abc import Awaitable, Callable, Sequence
from typing import Protocol, runtime_checkable

from agentchat.models import (
    Answer,
    Attachment,
    DownloadedFile,
    Message,
    Prompt,
    RichReply,
    UploadedFile,
    UploadFile,
)

MessageReceiver = Callable[["Channel", Message], Awaitable[None]]


class Channel(Protocol):
    """Messaging adapter consumed by AgentChat; no inheritance is required.

    Bind a receiver before running, emit normalized messages with stable IDs,
    and return confirmed replies in the source conversation. Transport failures
    propagate to the caller. The adapter owns connection resources and cleanup;
    AgentChat owns deduplication, conversation ordering, and local history.
    """

    name: str

    def bind(self, receiver: MessageReceiver) -> None: ...

    async def run(self) -> None: ...

    async def close(self) -> None: ...

    async def reply(self, source: Message, content: str) -> Message: ...


@runtime_checkable
class ThreadHistoryChannel(Protocol):
    """Optional channel capability: authoritative thread messages before the current turn."""

    async def thread_history(self, source: Message, *, limit: int = 50) -> tuple[Message, ...]: ...


@runtime_checkable
class MirroringChannel(Protocol):
    """Optional transport for an authorized user input from another surface."""

    async def mirror(self, source: Message, message: Message, *, origin: str) -> Message: ...


@runtime_checkable
class RichReplyChannel(Protocol):
    """Optional replies with fallback text and provider-compatible rich content."""

    async def reply_rich(self, source: Message, content: RichReply) -> Message: ...


@runtime_checkable
class FileUploadChannel(Protocol):
    """Optional uploads of host-authorized bytes to the source conversation."""

    async def upload_files(
        self, source: Message, files: Sequence[UploadFile],
    ) -> tuple[UploadedFile, ...]: ...


@runtime_checkable
class FileDownloadChannel(Protocol):
    """Optional explicit download of an attachment belonging to the source message."""

    async def download_attachment(
        self, source: Message, attachment: Attachment, *, max_bytes: int = 10 * 1024 * 1024,
    ) -> DownloadedFile: ...


@runtime_checkable
class StreamingChannel(Protocol):
    """Optional answers shown while they are written.

    Each call carries the whole text of the current message so far. `start_stream`
    returns an opaque handle; pass it to the next call, which returns the handle to
    use after it. `stop_stream` confirms the finished message. A channel may set
    `stream_interval` (seconds between updates) and `stream_limit` (UTF-16 code
    units per message); longer answers continue in a new message.
    """

    async def start_stream(self, source: Message, text: str) -> object: ...

    async def append_stream(self, source: Message, stream: object, text: str) -> object: ...

    async def stop_stream(self, source: Message, stream: object, text: str) -> Message: ...


@runtime_checkable
class InteractiveChannel(Protocol):
    """Optional questions with buttons, for approvals and choices.

    `send_prompt` shows the prompt in the source conversation with one button per
    choice. Each button carries `agentchat.prompt_token(prompt.id, choice.id)`. A
    press arrives as an ordinary `Message` in the same conversation from the person
    who pressed, with `metadata["agentchat_answer"] = {"prompt": ..., "choice": ...}`.
    `close_prompt` removes the buttons once answered (`answer`) or expired (None);
    channels that cannot edit a sent message may do nothing.
    """

    async def send_prompt(self, source: Message, prompt: Prompt) -> Message: ...

    async def close_prompt(
        self, source: Message, sent: Message, prompt: Prompt, answer: Answer | None,
    ) -> None: ...


@runtime_checkable
class StatusChannel(Protocol):
    """Optional transient activity indicator, separate from conversation history."""

    async def set_status(self, source: Message, status: str) -> bool: ...
