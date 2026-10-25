from collections.abc import Collection

import socketio

from agentchat.channels.base import MessageReceiver
from agentchat.channels.imessage.transport import BlueBubblesClient
from agentchat.models import Message

TYPING_INTERVAL: float
TYPING_EXPIRY: float

class IMessage:
    name: str
    def __init__(
        self, *, server_url: str, password: str, instance_id: str | None = ...,
        allowed_chat_guids: Collection[str] | None = ..., client: BlueBubblesClient | None = ...,
        socket_client: socketio.AsyncClient | None = ..., max_pending_events: int = ...,
        shutdown_timeout: float = ...,
    ) -> None: ...
    @classmethod
    def from_env(cls) -> IMessage: ...
    def bind(self, receiver: MessageReceiver) -> None: ...
    async def run(self) -> None: ...
    async def close(self) -> None: ...
    async def reply(self, source: Message, content: str) -> Message: ...
    async def set_status(self, source: Message, status: str) -> bool: ...
    async def _on_message(self, event: object) -> None: ...
