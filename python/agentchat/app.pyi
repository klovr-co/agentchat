# Public compatibility signatures; update with the corresponding Rust binding API.
from collections.abc import Awaitable, Callable, Mapping, Sequence
from typing import Literal

from agentchat._backend import Backend
from agentchat.activity import WorkingStatus
from agentchat.channels.base import Channel
from agentchat.context import MessageContext
from agentchat.models import (
    Answer,
    Attachment,
    Choice,
    DownloadedFile,
    Message,
    RichReply,
    UploadedFile,
    UploadFile,
)
from agentchat.state.base import State
from agentchat.stream import AnswerStream

MessageHandler = Callable[[MessageContext], Awaitable[str | None]]
Busy = Literal["steer", "queue", "ask", "reject"]

class AgentChat:
    channels: tuple[Channel, ...]
    state: State

    def __init__(
        self,
        *,
        channels: Sequence[Channel],
        state: State | None = ...,
        backend: Backend | None = ...,
        busy: Busy = ...,
    ) -> None: ...
    @property
    def backend(self) -> str: ...
    @property
    def busy(self) -> Busy: ...
    def stop(self, conversation_id: str) -> bool: ...
    # A default expiry keeps an unanswered approval from waiting forever.
    async def ask(
        self, channel: Channel, source: Message, text: str, choices: Sequence[Choice | str], *,
        timeout: float = ...,  # noqa: ASYNC109
        allowed: Sequence[str] | Literal["anyone"] | None = ...,
    ) -> Answer: ...
    async def offer_stop(
        self, channel: Channel, source: Message, *, text: str = ..., label: str = ...,
        allowed: Sequence[str] | Literal["anyone"] | None = ...,
    ) -> Message | None: ...
    def on_message(self, handler: MessageHandler) -> MessageHandler: ...
    async def run(self) -> None: ...
    async def close(self) -> None: ...
    async def reply(self, channel: Channel, source: Message, content: str) -> Message: ...
    async def reply_rich(
        self, channel: Channel, source: Message, content: RichReply
    ) -> Message: ...
    async def upload_files(
        self, channel: Channel, source: Message, files: Sequence[UploadFile]
    ) -> tuple[UploadedFile, ...]: ...
    async def set_status(self, channel: Channel, source: Message, status: str) -> bool: ...
    async def download_attachment(
        self, channel: Channel, source: Message, attachment: Attachment, *, max_bytes: int = ...
    ) -> DownloadedFile: ...
    def stream(
        self, channel: Channel, source: Message, *, interval: float | None = ...
    ) -> AnswerStream: ...
    def working(
        self, channel: Channel, source: Message, status: str = ..., *, refresh_interval: float = ...
    ) -> WorkingStatus: ...
    async def mirror(
        self, channel: Channel, source: Message, message: Message, *, origin: str = ...
    ) -> Message: ...
    async def send_ephemeral(
        self, channel: Channel, source: Message, user_id: str, text: str, *,
        blocks: Sequence[Mapping[str, object]] | None = ...,
        fallback: Literal["dm"] | None = ...,
    ) -> Message: ...
    async def delete(
        self, channel: Channel, message: Message, *,
        fallback: Literal["edit", "notice"] | None = ..., text: str | None = ...,
    ) -> Literal["deleted", "edited", "noticed"]: ...
    async def edit(self, channel: Channel, message: Message, text: str) -> Message: ...
    async def open_form(
        self, channel: Channel, source: Message, form: Mapping[str, object]
    ) -> None: ...
