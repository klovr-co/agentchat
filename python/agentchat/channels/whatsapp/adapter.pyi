# Public compatibility signatures; update with the corresponding Rust binding API.
from typing import Literal

from aiohttp import web

from agentchat.channels.base import MessageReceiver
from agentchat.models import Answer, Message, Prompt

GRAPH_ROOT: str
TYPING_INTERVAL: float
TYPING_EXPIRY: float

class WhatsApp:
    name: str

    def __init__(
        self,
        *,
        phone_number_id: str,
        access_token: str,
        app_secret: str,
        verify_token: str,
        api_version: str = ...,
        host: str = ...,
        port: int = ...,
        max_pending_events: int = ...,
        shutdown_timeout: float = ...,
        prompt_style: Literal["reactions", "buttons"] = ...,
    ) -> None: ...
    @classmethod
    def from_env(cls) -> WhatsApp: ...
    def bind(self, receiver: MessageReceiver) -> None: ...
    def create_app(self) -> web.Application: ...
    async def run(self) -> None: ...
    async def close(self) -> None: ...
    async def reply(self, source: Message, content: str) -> Message: ...
    async def send_prompt(self, source: Message, prompt: Prompt) -> Message: ...
    async def close_prompt(
        self, source: Message, sent: Message, prompt: Prompt, answer: Answer | None,
    ) -> None: ...
    async def set_status(self, source: Message, status: str) -> bool: ...
