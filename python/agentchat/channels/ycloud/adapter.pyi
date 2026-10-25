# Public compatibility signatures; update with the corresponding Rust binding API.
import re
from collections.abc import Awaitable, Callable, Mapping, Sequence
from typing import ClassVar, NamedTuple, TypeVar

from aiohttp import web

from agentchat.channels.base import AppHandler, MessageReceiver
from agentchat.models import Answer, DownloadedFile, Message, Prompt, RichReply

API_ROOT: str
TYPING_INTERVAL: float
TYPING_EXPIRY: float

_Handler = TypeVar("_Handler", bound=AppHandler)

class WhatsAppApiError(NamedTuple):
    code: str | None
    message: str | None
    type: str | None
    error_data: object

class YCloudStatus(NamedTuple):
    event_id: str | None
    status: str
    timestamp: str | None
    message_id: str | None
    wamid: str | None
    external_id: str | None
    recipient: str | None
    conversation_id: str | None
    error_code: str | None
    error_message: str | None
    error_kind: str | None
    whatsapp_api_error: WhatsAppApiError | None

YCloudEvent = YCloudStatus | Message
YCloudEventHandler = Callable[[YCloud, YCloudEvent], Awaitable[object]]

class YCloudTemplate(NamedTuple):
    name: str
    language: str
    category: str | None
    status: str | None
    body: str | None
    components: list

class YCloudError(RuntimeError):
    WINDOW_EXPIRED: ClassVar[str]
    TEMPLATE_PARAMETERS: ClassVar[str]
    TEMPLATE_NOT_FOUND: ClassVar[str]
    TEMPLATE_UNAVAILABLE: ClassVar[str]
    BALANCE_INSUFFICIENT: ClassVar[str]
    UNAUTHORIZED: ClassVar[str]
    RATE_LIMITED: ClassVar[str]
    REJECTED: ClassVar[str]
    UNCERTAIN: ClassVar[str]
    MEDIA_TOO_LARGE: ClassVar[str]
    MEDIA_INTEGRITY: ClassVar[str]
    MEDIA_UNAVAILABLE: ClassVar[str]
    kind: str
    status: int | None
    code: str | None
    whatsapp_code: str | None
    request_id: str | None

    def __init__(
        self,
        description: str,
        *,
        kind: str,
        status: int | None = ...,
        code: str | None = ...,
        whatsapp_code: str | None = ...,
        request_id: str | None = ...,
    ) -> None: ...

class YCloud:
    name: str

    def __init__(
        self,
        *,
        phone_number: str,
        api_key: str,
        webhook_secret: str,
        host: str = ...,
        port: int = ...,
        smb_echoes: bool = ...,
        max_media_bytes: int = ...,
        max_pending_events: int = ...,
        shutdown_timeout: float = ...,
    ) -> None: ...
    @classmethod
    def from_env(cls) -> YCloud: ...
    def bind(self, receiver: MessageReceiver) -> None: ...
    def on_event(self, event_type: str) -> Callable[[_Handler], _Handler]: ...
    def on_action(self, action_id: str | re.Pattern[str]) -> Callable[[_Handler], _Handler]: ...
    def on_view(self, callback_id: str | re.Pattern[str]) -> Callable[[_Handler], _Handler]: ...
    def on_command(self, command: str) -> Callable[[_Handler], _Handler]: ...
    def on_raw(self, handler: _Handler) -> _Handler: ...
    async def send_ephemeral(
        self, source: Message, user_id: str, text: str, *,
        blocks: Sequence[Mapping[str, object]] | None = ...,
    ) -> Message: ...
    async def reply_rich(self, source: Message, content: RichReply) -> Message: ...
    def bind_events(self, handler: YCloudEventHandler) -> None: ...
    def create_app(self) -> web.Application: ...
    async def run(self) -> None: ...
    async def close(self) -> None: ...
    async def reply(
        self, source: Message, content: str, *, external_id: str | None = ...
    ) -> Message: ...
    async def set_status(self, source: Message, status: str) -> bool: ...
    async def send_prompt(self, source: Message, prompt: Prompt) -> Message: ...
    async def close_prompt(
        self, source: Message, sent: Message, prompt: Prompt, answer: Answer | None,
    ) -> None: ...
    async def send_template(
        self,
        destination: str,
        name: str,
        language_code: str,
        components: list[Mapping[str, object]] | None = ...,
        *,
        external_id: str | None = ...,
    ) -> Message: ...
    async def list_templates(
        self, status: str | None = ..., language: str | None = ...
    ) -> list[YCloudTemplate]: ...
    async def download_media(
        self, media: Message | Mapping[str, object], *, max_bytes: int | None = ...
    ) -> DownloadedFile: ...
