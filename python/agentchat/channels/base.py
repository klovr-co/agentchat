from __future__ import annotations

from collections.abc import Awaitable, Callable, Mapping, Sequence
from typing import Any, Protocol, runtime_checkable

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
AppPayload = Mapping[str, Any]
"""A raw provider payload that AgentChat does not turn into a message."""
AppHandler = Callable[[AppPayload], Awaitable[Mapping[str, Any] | None]]
"""Handles an `AppPayload`; where the provider takes an answer, the result is it."""


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
class TaskProgressChannel(Protocol):
    """Optional task and plan progress shown with an answer.

    `update_stream_tasks` receives the unsent updates in order, each a mapping:
    `{"type": "plan", "title": ...}` or `{"type": "task", "id": ..., "title": ...,
    "status": "pending" | "in_progress" | "complete" | "error", "details": ...,
    "output": ...}`. `stream` is None on the first call; afterwards it is the handle
    the previous call returned. It returns the handle to use next and may drop
    updates it can no longer show. On a `StreamingChannel` the handle is the
    stream's: with `stream` None it starts the stream with the updates. Channels
    without it ignore `AnswerStream.task` and `plan`.
    """

    async def update_stream_tasks(
        self, source: Message, stream: object | None, updates: Sequence[Mapping[str, object]],
    ) -> object: ...


@runtime_checkable
class TaskStreamingChannel(StreamingChannel, TaskProgressChannel, Protocol):
    """Task and plan progress shown with a streamed answer; see `TaskProgressChannel`."""


@runtime_checkable
class HomeChannel(Protocol):
    """Optional home screen for one person, replaced by each `publish_home`.

    Slack publishes an App Home view. Other channels keep one home message in the
    person's direct chat and edit it in place where they can.
    """

    async def publish_home(self, user_id: str, view: Any) -> object: ...


@runtime_checkable
class EphemeralChannel(Protocol):
    """Optional notices in the source conversation that only one person can see.

    A channel without truly private notices must not implement this; AgentChat
    then raises `TypeError` rather than posting publicly.
    """

    async def send_ephemeral(
        self, source: Message, user_id: str, text: str, *,
        blocks: Sequence[Mapping[str, object]] | None = None,
    ) -> Message: ...


@runtime_checkable
class DeletingChannel(Protocol):
    """Optional deletion of a message this application sent."""

    async def delete(self, message: Message) -> None: ...


@runtime_checkable
class EditingChannel(Protocol):
    """Optional replacement of the text of a message this application sent."""

    async def edit(self, message: Message, text: str) -> Message: ...


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
