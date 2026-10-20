# Public compatibility signatures; update with the corresponding Rust binding API.
from aiohttp import web

from agentchat.channels.base import MessageReceiver
from agentchat.models import Message

API_ROOT: str

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
        max_pending_events: int = ...,
        shutdown_timeout: float = ...,
    ) -> None: ...
    @classmethod
    def from_env(cls) -> YCloud: ...
    def bind(self, receiver: MessageReceiver) -> None: ...
    def create_app(self) -> web.Application: ...
    async def run(self) -> None: ...
    async def close(self) -> None: ...
    async def reply(self, source: Message, content: str) -> Message: ...
