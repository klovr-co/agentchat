"""iMessage socket events, commands, the stop keyword and notices."""

import asyncio

import pytest

from agentchat import AgentChat
from agentchat.channels.imessage import IMessage
from agentchat.channels.imessage.events import normalize
from tests.support.imessage import BubblesClient, SocketClient, imessage_event
from tests.support.receipts import Receipt


class Sockets(SocketClient):
    def __init__(self):
        super().__init__()
        self.handlers = {}

    def on(self, event, callback):
        self.handlers[event] = callback
        if event == "new-message":
            super().on(event, callback)


def channel_with(**options):
    socket = Sockets()
    channel = IMessage(server_url="http://localhost:1234", password="fake", instance_id="mac",
                       client=BubblesClient(Receipt("sent")), socket_client=socket, **options)
    return channel, socket


async def until(predicate):
    for _ in range(400):
        if predicate():
            return
        await asyncio.sleep(0.005)
    raise AssertionError("Condition was not reached")


@pytest.mark.asyncio
async def test_socket_events_and_commands_reach_handlers():
    channel, socket = channel_with()
    seen, messages = [], []

    async def receive(channel, message):
        messages.append(message.text)

    channel.bind(receive)

    @channel.on_event("updated-message")
    async def updated(payload):
        seen.append((payload["event"], payload["data"]["guid"]))

    @channel.on_command("/status")
    async def status(payload):
        seen.append(("command", payload["text"]))

    await socket.handlers["updated-message"]({"guid": "g1"})
    await socket.handlers["new-message"](imessage_event(text="/status now", guid="c1"))
    await socket.handlers["new-message"](imessage_event(text="hello", guid="c2"))
    await until(lambda: len(seen) == 2 and messages)
    assert sorted(seen) == [("command", "/status now"), ("updated-message", "g1")]
    assert messages == ["hello"]
    await channel.close()


@pytest.mark.asyncio
async def test_stop_keyword_stops_the_senders_turn_and_notices_need_a_direct_chat():
    channel, socket = channel_with(stop_keyword="stop")
    app = AgentChat(channels=[channel])
    started, stopped = asyncio.Event(), asyncio.Event()

    @app.on_message
    async def respond(context):
        started.set()
        await asyncio.wait_for(context.wait_stopped(), 2)
        stopped.set()

    await socket.handlers["new-message"](imessage_event(text="work", guid="w1"))
    await asyncio.wait_for(started.wait(), 2)
    await socket.handlers["new-message"](imessage_event(text=" STOP ", guid="w2"))
    await asyncio.wait_for(stopped.wait(), 2)

    direct = normalize(imessage_event(), instance_id="mac")
    group = normalize(imessage_event(chats=[{"guid": "iMessage;+;chat1"}]), instance_id="mac")
    notice = await app.send_ephemeral(channel, direct, "+15551234567", "Only you")
    assert notice.text == "Only you"
    with pytest.raises(ValueError):
        await app.send_ephemeral(channel, group, "+15551234567", "Only you")
    await app.close()
