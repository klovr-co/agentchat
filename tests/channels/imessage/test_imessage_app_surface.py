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


class EditingClient(BubblesClient):
    """A server with the Private API: messages can be edited and unsent."""

    def __init__(self):
        super().__init__(Receipt("sent"))
        self.sent, self.edits, self.unsent = [], [], []

    async def send_text(self, chat_guid, text):
        self.sent.append((chat_guid, text))
        return f"g{len(self.sent)}"

    async def edit_text(self, guid, text):
        self.edits.append((guid, text))

    async def unsend(self, guid):
        self.unsent.append(guid)


def editing_channel(**options):
    client = EditingClient()
    channel = IMessage(server_url="http://localhost:1234", password="fake", instance_id="mac",
                       client=client, socket_client=Sockets(), **options)
    return channel, client


@pytest.mark.asyncio
async def test_a_command_handlers_text_answer_is_sent_as_a_reply():
    channel, client = editing_channel()
    channel.bind(lambda channel, message: asyncio.sleep(0))

    @channel.on_command("/status")
    async def status(payload):
        return {"text": "All good"}

    await channel._socket.handlers["new-message"](imessage_event(text="/status", guid="c1"))
    await until(lambda: client.sent)
    assert client.sent == [("iMessage;-;+15551234567", "All good")]
    await channel.close()


@pytest.mark.asyncio
async def test_unsend_progress_edits_and_notices_by_direct_message():
    channel, client = editing_channel()
    app = AgentChat(channels=[channel])
    direct = normalize(imessage_event(), instance_id="mac")
    group = normalize(imessage_event(chats=[{"guid": "iMessage;+;chat1"}]), instance_id="mac")

    sent = await channel.reply(direct, "Temporary")
    await app.delete(channel, sent)
    assert client.unsent == ["g1"]
    with pytest.raises(ValueError):
        await app.delete(channel, direct)

    notice = await app.send_ephemeral(channel, group, "+15550000000", "Only you", fallback="dm")
    assert client.sent[-1] == ("iMessage;-;+15550000000", "Only you")
    assert notice.metadata["chat_guid"] == "iMessage;-;+15550000000"
    with pytest.raises(ValueError):
        await app.send_ephemeral(channel, group, "+15550000000", "Only you", fallback="email")

    async with app.stream(channel, direct, interval=0) as reply:
        await reply.plan("Checking")
        await reply.task("a", "Read the logs")
        await reply.task("a", "Read the logs", "complete")
        await reply.append("Done.")
    progress = [text for chat, text in client.sent if text.startswith("**Checking**")]
    assert progress == ["**Checking**"]
    assert client.edits[-1][1] == "**Checking**\n✅ Read the logs"
    assert client.sent[-1][1] == "Done."
    history = await app.state.history(direct.conversation_id)
    assert [message.text for message in history] == ["Done."]
    await app.close()


@pytest.mark.asyncio
async def test_home_message_is_edited_until_apple_stops_allowing_it():
    channel, client = editing_channel()
    first = await channel.publish_home("+15550000000", "Home 1")
    assert client.sent == [("iMessage;-;+15550000000", "Home 1")]
    for number in range(2, 7):
        home = await channel.publish_home("+15550000000", f"Home {number}")
        assert home.id == first.id
    assert len(client.edits) == 5
    # Apple allows five edits; the sixth change is a new message.
    await channel.publish_home("+15550000000", "Home 7")
    assert client.sent[-1] == ("iMessage;-;+15550000000", "Home 7")
    await channel.close()
