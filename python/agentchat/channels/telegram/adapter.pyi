# Public compatibility signatures; update with the corresponding Rust binding API.
import re
from collections.abc import Callable, Mapping, Sequence
from typing import Any, Literal, Protocol, TypeVar

from aiohttp import web

from agentchat.channels.base import AppHandler, MessageReceiver
from agentchat.channels.telegram.transport import TelegramFile
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

ENV_BOT_TOKEN: str
ENV_WEBHOOK_URL: str
ENV_WEBHOOK_SECRET: str
ENV_HOST: str
ENV_PORT: str
DEFAULT_HOST: str
DEFAULT_PORT: int
POLL_TIMEOUT: int
POLL_REQUEST_TIMEOUT: int
ALLOWED_UPDATES: list[str]
WEBHOOK_PATH: str
SECRET_HEADER: str
RETRY_BASE: float
RETRY_MAX: float
BUSY_DELAY: float
OFFSET_CONFIRM_TIMEOUT: int
CALLBACK_ANSWER_TIMEOUT: int
TYPING_INTERVAL: int
TYPING_EXPIRY: int
CHAT_ACTION_TYPING: str
CALLBACK_ONLY_METADATA: frozenset[str]

_Handler = TypeVar("_Handler", bound=AppHandler)

class BotAPIClient(Protocol):
    async def call(
        self, method: str, payload: dict | None = ..., *, request_timeout: float = ...
    ) -> Any: ...
    async def upload(
        self, *, method: str, fields: dict, file_field: str, filename: str, content: bytes
    ) -> Any: ...
    async def download_file(self, file_id: str, *, max_bytes: int) -> TelegramFile: ...
    async def close(self) -> None: ...

def retry_delay(*, attempt: int, retry_after: int | None) -> float: ...

class Telegram:
    name: str

    def __init__(
        self,
        *,
        bot_token: str,
        webhook_url: str | None = ...,
        webhook_secret: str | None = ...,
        host: str = ...,
        port: int = ...,
        client: BotAPIClient | None = ...,
        max_pending_events: int = ...,
        shutdown_timeout: float = ...,
        stream_drafts: bool = ...,
    ) -> None: ...
    @classmethod
    def from_env(cls) -> Telegram: ...
    def bind(self, receiver: MessageReceiver) -> None: ...
    def on_event(self, event_type: str) -> Callable[[_Handler], _Handler]: ...
    def on_action(self, action_id: str | re.Pattern[str]) -> Callable[[_Handler], _Handler]: ...
    def on_view(self, callback_id: str | re.Pattern[str]) -> Callable[[_Handler], _Handler]: ...
    def on_command(self, command: str) -> Callable[[_Handler], _Handler]: ...
    def on_raw(self, handler: _Handler) -> _Handler: ...
    async def send_ephemeral(
        self, source: Message, user_id: str, text: str, *,
        blocks: Sequence[Mapping[str, object]] | None = ...,
        fallback: Literal["dm"] | None = ...,
    ) -> Message: ...
    async def publish_home(self, user_id: str, view: str) -> Message: ...
    async def delete(self, message: Message) -> None: ...
    async def edit(self, message: Message, text: str) -> Message: ...
    async def update_stream_tasks(
        self, source: Message, stream: object | None, updates: Sequence[Mapping[str, object]],
    ) -> object: ...
    def create_app(self) -> web.Application: ...
    async def run(self) -> None: ...
    async def close(self) -> None: ...
    async def reply(self, source: Message, content: str) -> Message: ...
    stream_interval: float
    async def start_stream(self, source: Message, text: str) -> object: ...
    async def append_stream(self, source: Message, stream: object, text: str) -> object: ...
    async def stop_stream(self, source: Message, stream: object, text: str) -> Message: ...
    stream_limit: int
    async def send_prompt(self, source: Message, prompt: Prompt) -> Message: ...
    async def close_prompt(
        self, source: Message, sent: Message, prompt: Prompt, answer: Answer | None,
    ) -> None: ...
    async def reply_rich(self, source: Message, content: RichReply) -> Message: ...
    async def set_status(self, source: Message, status: str) -> bool: ...
    async def upload_files(
        self, source: Message, files: Sequence[UploadFile]
    ) -> tuple[UploadedFile, ...]: ...
    async def download_attachment(
        self, source: Message, attachment: Attachment, *, max_bytes: int = ...
    ) -> DownloadedFile: ...
