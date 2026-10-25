"""Each provider shows a streamed answer its own way and confirms one message."""
from dataclasses import replace
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from microsoft_agents.activity import ResourceResponse
from microsoft_agents.hosting.aiohttp import CloudAdapter
from microsoft_agents.hosting.core import OutboundHostValidator

from agentchat import AgentChat
from agentchat.channels.base import StreamingChannel
from agentchat.channels.discord import Discord
from agentchat.channels.discord.events import normalize as discord_message
from agentchat.channels.slack import Slack
from agentchat.channels.teams import Teams
from agentchat.channels.teams.events import normalize as teams_message
from agentchat.channels.whatsapp import WhatsApp
from agentchat.channels.whatsapp.events import normalize as whatsapp_messages
from tests.support.discord import DiscordClient, discord_event
from tests.support.receipts import Receipt
from tests.support.slack import FakeSlackClient, slack_source
from tests.support.teams import APP_ID, TENANT_ID, teams_event
from tests.support.telegram import TelegramClient, faked, read, telegram_update
from tests.support.whatsapp import WhatsAppSession, whatsapp_event

OPTIONS = {"max_pending_events": 2, "shutdown_timeout": 0.01}


async def stream(channel, source, parts):
    app = AgentChat(channels=[channel])
    async with app.stream(channel, source, interval=0) as answer:
        for part in parts:
            await answer.append(part)
    history = await app.state.history(source.conversation_id)
    return answer, history


def slack_thread(thread="1700000000.000100"):
    source = slack_source(thread)
    metadata = {**source.metadata, "channel_id": "C0123ABCD", "reply_thread_timestamp": thread}
    return replace(source, metadata=metadata)


@pytest.mark.asyncio
async def test_slack_streams_natively_in_a_thread():
    client = FakeSlackClient()
    slack = Slack(bot_token="test", app_token="test", web_client=client)
    assert isinstance(slack, StreamingChannel)
    answer, history = await stream(slack, slack_thread(), ["Hel", "lo"])
    assert client.api_calls == [
        ("chat.startStream", {
            "channel": "C0123ABCD", "thread_ts": "1700000000.000100", "markdown_text": "Hel",
            "recipient_user_id": "U", "recipient_team_id": "T",
        }),
        ("chat.appendStream", {"channel": "C0123ABCD", "ts": "5.0", "markdown_text": "lo"}),
        ("chat.stopStream", {"channel": "C0123ABCD", "ts": "5.0"}),
    ]
    assert client.posts == []
    message = answer.messages[0]
    assert (message.text, message.role, message.metadata["message_timestamp"]) == (
        "Hello", "assistant", "5.0",
    )
    assert history == answer.messages


@pytest.mark.asyncio
async def test_slack_outside_a_thread_posts_once_at_the_end():
    client = FakeSlackClient()
    slack = Slack(bot_token="test", app_token="test", web_client=client)
    source = slack_thread(thread=None)
    answer, _ = await stream(slack, source, ["Hel", "lo"])
    assert client.api_calls == []
    assert [post["text"] for post in client.posts] == ["Hello"]
    assert answer.messages[0].text == "Hello"


@pytest.mark.asyncio
async def test_slack_client_without_api_call_posts_once():
    class PostOnly:
        def __init__(self):
            self.posts = []

        async def chat_postMessage(self, **kwargs):
            self.posts.append(kwargs)
            return {"ok": True, "ts": "7.0"}

    client = PostOnly()
    slack = Slack(bot_token="test", app_token="test", web_client=client)
    await stream(slack, slack_thread(), ["a", "b"])
    assert [post["text"] for post in client.posts] == ["ab"]


@pytest.mark.asyncio
async def test_discord_posts_then_edits_and_splits_at_its_limit():
    client = DiscordClient(Receipt(101))
    channel = Discord(bot_token="fake", client=client, send_client=client, **OPTIONS)
    source = discord_message(discord_event(), bot_id=99)
    try:
        answer, history = await stream(channel, source, ["Hel", "lo"])
        assert [args[1]["content"] for args, _ in client.receipt.calls] == ["Hel"]
        assert client.edits == [(10, 101, "Hello")]
        assert answer.messages[0].metadata["message_id"] == 101
        assert history == answer.messages
        client.receipt.calls.clear()
        client.edits.clear()
        answer, _ = await stream(channel, source, ["word " * 500])
        assert len(answer.messages) == 2
        assert all(len(m.text) <= 2000 for m in answer.messages)
        assert "".join(m.text for m in answer.messages) == "word " * 500
    finally:
        await channel.close()


@pytest.mark.asyncio
async def test_telegram_sends_then_edits_in_place():
    client = TelegramClient()
    channel = faked(client)
    source = read(telegram_update())
    try:
        answer, _ = await stream(channel, source, ["Hel", "lo"])
        assert [p["text"] for p in client.method_calls("sendMessage")] == ["Hel"]
        edits = client.method_calls("editMessageText")
        assert [(e["chat_id"], e["message_id"], e["text"]) for e in edits] == [
            (30, 101, "Hello"),
        ]
        assert answer.messages[0].metadata["message_id"] == 101
    finally:
        await channel.close()


def teams_channel():
    calls = []

    async def send(*args):
        calls.append(("send", args[-1]))
        return ResourceResponse.model_construct(id=f"activity-{len(calls)}")

    async def update(*args):
        calls.append(("update", args[-1]))
        return ResourceResponse.model_construct(id=args[1])

    connector = SimpleNamespace(
        conversations=SimpleNamespace(
            send_to_conversation=send, reply_to_activity=send, update_activity=update,
        ),
        close=AsyncMock(),
    )
    factory = SimpleNamespace(
        create_connector_client=AsyncMock(return_value=connector),
        create_user_token_client=AsyncMock(return_value=SimpleNamespace(close=AsyncMock())),
    )
    adapter = CloudAdapter(
        channel_service_client_factory=factory,
        host_validator=OutboundHostValidator(enabled=True),
    )
    channel = Teams(
        app_id=APP_ID, tenant_id=TENANT_ID, app_password="fake", adapter=adapter, port=0, **OPTIONS
    )
    return channel, calls


def streaminfo(activity):
    entities = [e.model_dump(by_alias=True, exclude_none=True) for e in activity.entities or []]
    return [
        {k: v for k, v in e.items() if k in ("streamType", "streamSequence", "streamId")}
        for e in entities if e.get("type") == "streaminfo"
    ]


@pytest.mark.asyncio
async def test_teams_streams_natively_in_one_to_one_chats():
    channel, calls = teams_channel()
    source = teams_message(teams_event(kind="personal"), app_id=APP_ID, tenant_id=TENANT_ID)
    try:
        answer, _ = await stream(channel, source, ["Hel", "lo"])
        assert [(kind, a.type, a.text) for kind, a in calls] == [
            ("send", "typing", "Hel"), ("send", "typing", "Hello"), ("send", "message", "Hello"),
        ]
        assert streaminfo(calls[0][1]) == [
            {"streamType": "streaming", "streamSequence": 1, "streamId": ""},
        ]
        assert streaminfo(calls[1][1]) == [
            {"streamType": "streaming", "streamSequence": 2, "streamId": "activity-1"},
        ]
        assert streaminfo(calls[-1][1])[0]["streamType"] == "final"
        assert streaminfo(calls[-1][1])[0]["streamId"] == "activity-1"
        assert answer.messages[0].text == "Hello"
    finally:
        await channel.close()


@pytest.mark.asyncio
async def test_teams_updates_one_message_in_channels():
    channel, calls = teams_channel()
    event = teams_event(kind="channel", replyToId="root",
                        entities=[{"type": "mention", "text": "<at>Bot</at>",
                                   "mentioned": {"id": f"28:{APP_ID}"}}])
    source = teams_message(event, app_id=APP_ID, tenant_id=TENANT_ID)
    try:
        answer, _ = await stream(channel, source, ["Hel", "lo"])
        assert [(kind, a.type, a.text) for kind, a in calls] == [
            ("send", "message", "Hel"), ("update", "message", "Hello"),
            ("update", "message", "Hello"),
        ]
        assert all(a.id == "activity-1" for kind, a in calls if kind == "update")
        assert answer.messages[0].text == "Hello"
    finally:
        await channel.close()


@pytest.mark.asyncio
async def test_whatsapp_cannot_edit_so_it_replies_once():
    receipt = Receipt("sent")
    channel = WhatsApp(
        phone_number_id="123", access_token="fake", app_secret="secret",
        verify_token="verify", port=0, **OPTIONS,
    )
    channel._session = WhatsAppSession(receipt)
    assert not isinstance(channel, StreamingChannel)
    source = whatsapp_messages(whatsapp_event(), phone_number_id="123")[0]
    try:
        answer, _ = await stream(channel, source, ["Hel", "lo"])
        assert [kwargs["json"]["text"]["body"] for _, kwargs in receipt.calls] == ["Hello"]
        assert answer.messages[0].text == "Hello"
    finally:
        await channel.close()


@pytest.mark.asyncio
async def test_streams_refuse_a_source_from_another_provider():
    client = DiscordClient(Receipt(101))
    channel = Discord(bot_token="fake", client=client, send_client=client, **OPTIONS)
    source = replace(discord_message(discord_event(), bot_id=99), channel="other")
    try:
        with pytest.raises(ValueError):
            await stream(channel, source, ["text"])
        assert client.receipt.calls == []
    finally:
        await channel.close()
