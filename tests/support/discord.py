import asyncio
from types import SimpleNamespace
from unittest.mock import Mock

import discord


def discord_event(*, direct=True, thread=False, **changes):
    channel = Mock(spec=discord.Thread) if thread else SimpleNamespace()
    channel.id = 10
    event = Mock(spec=discord.Message)
    event.id, event.channel = 100, channel
    event.guild = None if direct else SimpleNamespace(id=20)
    event.author = SimpleNamespace(id=30, bot=False, display_name="Alice")
    event.webhook_id, event.reference = None, None
    event.content, event.mentions, event.attachments = "hello", [], []
    event.is_system.return_value = False
    for key, value in changes.items():
        setattr(event, key, value)
    return event


class DiscordClient:
    def __init__(self, receipt):
        self.receipt = receipt
        self.user = SimpleNamespace(id=99, display_name="Bot")
        self.stopped, self.started = asyncio.Event(), asyncio.Event()
        self.closed = False
        self.typing, self.typing_error = [], None
        self.edits = []

    async def start(self, token):
        self.started.set()
        await self.stopped.wait()

    async def close(self):
        self.closed = True
        self.stopped.set()

    def is_closed(self):
        return self.closed

    async def send_text(self, channel_id, payload):
        return self.receipt.get(channel_id, payload)

    async def trigger_typing(self, channel_id):
        self.typing.append(channel_id)
        if self.typing_error:
            raise self.typing_error

    async def edit_text(self, channel_id, message_id, payload):
        self.edits.append((channel_id, message_id, payload["content"]))
