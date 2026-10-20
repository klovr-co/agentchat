# Public compatibility signatures; update with the corresponding Rust binding API.
from collections.abc import Awaitable, Callable, Mapping
from typing import ClassVar, NamedTuple

from aiohttp import web

from agentchat.channels.base import MessageReceiver
from agentchat.models import DownloadedFile, Message

API_ROOT: str

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
    def bind_events(self, handler: YCloudEventHandler) -> None: ...
    def create_app(self) -> web.Application: ...
    async def run(self) -> None: ...
    async def close(self) -> None: ...
    async def reply(
        self, source: Message, content: str, *, external_id: str | None = ...
    ) -> Message: ...
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
