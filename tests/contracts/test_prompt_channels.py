"""Each provider shows prompt buttons its own way and turns presses into answers."""
import asyncio
import hashlib
import hmac
import json
from dataclasses import replace
from types import SimpleNamespace
from unittest.mock import AsyncMock

import discord
import pytest
from aiohttp.test_utils import TestClient, TestServer
from microsoft_agents.activity import ResourceResponse
from microsoft_agents.hosting.aiohttp import CloudAdapter
from microsoft_agents.hosting.core import OutboundHostValidator

from agentchat import AgentChat, Choice
from agentchat.channels.base import InteractiveChannel
from agentchat.channels.discord import Discord
from agentchat.channels.discord.events import normalize as discord_message
from agentchat.channels.slack import Slack
from agentchat.channels.teams import Teams
from agentchat.channels.teams.events import normalize as teams_message
from agentchat.channels.whatsapp import WhatsApp
from agentchat.channels.whatsapp.events import normalize as whatsapp_messages
from agentchat.channels.ycloud import YCloud
from agentchat.channels.ycloud.events import normalize as ycloud_messages
from tests.support.discord import DiscordClient, discord_event
from tests.support.receipts import Receipt
from tests.support.slack import FakeSlackClient, slack_source
from tests.support.teams import APP_ID, TENANT_ID, teams_event
from tests.support.telegram import (
    TelegramClient,
    callback_update,
    faked,
    read,
    telegram_update,
)
from tests.support.whatsapp import WhatsAppSession, whatsapp_event
from tests.support.ycloud import YCloudSession, ycloud_event, ycloud_message

OPTIONS = {"max_pending_events": 2, "shutdown_timeout": 0.01}
CHOICES = [Choice("yes", "Approve", "primary"), Choice("no", "Deny", "danger")]


async def until(predicate):
    for _ in range(500):
        if predicate():
            return
        await asyncio.sleep(0.001)
    raise AssertionError("condition not reached")


def start_ask(channel, source):
    app = AgentChat(channels=[channel])
    asked = asyncio.create_task(app.ask(channel, source, "Delete build/?", CHOICES))
    return app, asked


@pytest.mark.asyncio
async def test_telegram_inline_buttons_round_trip():
    client = TelegramClient()
    channel = faked(client)
    assert isinstance(channel, InteractiveChannel)
    source = read(telegram_update())
    app, asked = start_ask(channel, source)
    try:
        await until(lambda: client.method_calls("sendMessage"))
        sent = client.method_calls("sendMessage")[0]
        buttons = sent["reply_markup"]["inline_keyboard"][0]
        assert [b["text"] for b in buttons] == ["Approve", "Deny"]
        token = buttons[0]["callback_data"]
        assert token.startswith("ac:") and len(token) <= 64
        press = read(callback_update(data=token, message={
            "message_id": 101, "date": 1700000000, "from": {"id": 99, "is_bot": True},
            "chat": {"id": 30, "type": "private", "first_name": "Alice"},
            "text": "Delete build/?", "reply_markup": sent["reply_markup"],
        }))
        assert press.metadata["agentchat_answer"]["choice"] == "yes"
        await app._receive(channel, press)
        answer = await asked
        assert (answer.choice, answer.label) == ("yes", "Approve")
        edit = client.method_calls("editMessageText")[0]
        assert (edit["message_id"], edit["text"].startswith("Delete build/?")) == (101, True)
        assert "✓ Approve" in edit["text"]
    finally:
        await app.close()


@pytest.mark.asyncio
async def test_telegram_other_callbacks_stay_ordinary_messages():
    press = read(callback_update(data="yes"))
    assert "agentchat_answer" not in press.metadata


def interaction(token, *, user_id=30, kind=discord.InteractionType.component):
    return SimpleNamespace(
        id=555, type=kind, data={"custom_id": token, "component_type": 2},
        user=SimpleNamespace(id=user_id, display_name="Alice", bot=False),
        channel_id=10, guild_id=None, channel=SimpleNamespace(),
        message=SimpleNamespace(id=101),
        response=SimpleNamespace(defer=AsyncMock()),
    )


@pytest.mark.asyncio
async def test_discord_buttons_round_trip_through_interactions():
    client = DiscordClient(Receipt(101))
    channel = Discord(bot_token="fake", client=client, send_client=client, **OPTIONS)
    assert isinstance(channel, InteractiveChannel)
    source = discord_message(discord_event(), bot_id=99)
    app, asked = start_ask(channel, source)
    try:
        await until(lambda: client.receipt.calls)
        payload = client.receipt.calls[0][0][1]
        row = payload["components"][0]["components"]
        assert [(b["label"], b["style"]) for b in row] == [("Approve", 1), ("Deny", 4)]
        other = interaction("not-ours")
        await channel._on_interaction(other)
        other.response.defer.assert_not_awaited()
        press = interaction(row[1]["custom_id"])
        await channel._on_interaction(press)
        press.response.defer.assert_awaited_once()
        answer = await asyncio.wait_for(asked, 1)
        assert (answer.choice, answer.sender.id) == ("no", "30")
        await until(lambda: client.edits)
        assert client.edits[0][:2] == (10, 101)
        assert "✓ Deny — Alice" in client.edits[0][2]
    finally:
        await app.close()


@pytest.mark.asyncio
async def test_slack_buttons_round_trip_through_block_actions():
    client = FakeSlackClient()
    updates = []

    async def chat_update(**kwargs):
        updates.append(kwargs)
        return {"ok": True}

    client.chat_update = chat_update
    slack = Slack(bot_token="test", app_token="test", web_client=client)
    assert isinstance(slack, InteractiveChannel)
    base = slack_source("1700000000.000100")
    source = replace(base, metadata={**base.metadata, "channel_id": "C0123ABCD"},
                     conversation_id="slack:T:C0123ABCD:1700000000.000100")
    app, asked = start_ask(slack, source)
    try:
        await until(lambda: client.posts)
        post = client.posts[0]
        assert post["thread_ts"] == "1700000000.000100"
        actions = post["blocks"][-1]["elements"]
        assert [(a["text"]["text"], a.get("style")) for a in actions] == [
            ("Approve", "primary"), ("Deny", "danger"),
        ]
        await slack.handle_event({
            "type": "block_actions",
            "team": {"id": "T"},
            "user": {"id": "U", "name": "alice"},
            "channel": {"id": "C0123ABCD"},
            "message": {"ts": "1", "thread_ts": "1700000000.000100"},
            "actions": [{"action_id": actions[0]["action_id"], "value": actions[0]["value"],
                         "text": {"type": "plain_text", "text": "Approve"},
                         "action_ts": "1700000001.000200"}],
        })
        answer = await asyncio.wait_for(asked, 1)
        assert (answer.choice, answer.sender.id) == ("yes", "U")
        assert updates[0]["ts"] == "1"
        assert "✓ Approve — alice" in updates[0]["text"]
        assert all(block["type"] != "actions" for block in updates[0]["blocks"])
    finally:
        await app.close()


@pytest.mark.asyncio
async def test_teams_cards_round_trip_through_submit_actions():
    calls = []

    async def send(*args):
        calls.append(("send", args[-1]))
        return ResourceResponse.model_construct(id="card-1")

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
    adapter = CloudAdapter(channel_service_client_factory=factory,
                           host_validator=OutboundHostValidator(enabled=True))
    channel = Teams(app_id=APP_ID, tenant_id=TENANT_ID, app_password="fake",
                    adapter=adapter, port=0, **OPTIONS)
    assert isinstance(channel, InteractiveChannel)
    source = teams_message(teams_event(), app_id=APP_ID, tenant_id=TENANT_ID)
    app, asked = start_ask(channel, source)
    try:
        await until(lambda: calls)
        card = calls[0][1].attachments[0].content
        actions = card["actions"]
        assert [(a["title"], a["style"]) for a in actions] == [
            ("Approve", "positive"), ("Deny", "destructive"),
        ]
        press = teams_message(teams_event(id="press", text=None, replyToId="card-1",
                                          value={"agentchat": actions[0]["data"]["agentchat"]}),
                              app_id=APP_ID, tenant_id=TENANT_ID)
        assert press.conversation_id == source.conversation_id
        await app._receive(channel, press)
        answer = await asyncio.wait_for(asked, 1)
        assert answer.choice == "yes"
        kind, closed = calls[-1]
        assert (kind, closed.id, closed.attachments) == ("update", "card-1", [])
        assert "✓ Approve — Alice" in closed.text
    finally:
        await app.close()


@pytest.mark.asyncio
async def test_teams_ignores_submits_without_a_prompt_token_outside_personal_chats():
    event = teams_event(kind="channel", text=None, value={"other": "data"})
    assert teams_message(event, app_id=APP_ID, tenant_id=TENANT_ID) is None


def whatsapp_channel(receipt, **options):
    channel = WhatsApp(phone_number_id="123", access_token="fake", app_secret="secret",
                       verify_token="verify", port=0, **OPTIONS, **options)
    channel._session = WhatsAppSession(receipt)
    return channel


def signed(payload):
    raw = json.dumps(payload).encode()
    signature = hmac.new(b"secret", raw, hashlib.sha256).hexdigest()
    return {"data": raw, "headers": {"X-Hub-Signature-256": f"sha256={signature}"}}


def reaction(emoji, target, *, sender="15551234567", reaction_id="wamid.reaction"):
    return whatsapp_event(messages=[{
        "id": reaction_id, "from": sender, "type": "reaction",
        "reaction": {"message_id": target, "emoji": emoji},
    }])


@pytest.mark.asyncio
async def test_whatsapp_prompts_with_reactions_when_chosen():
    receipt = Receipt("wamid.prompt")
    channel = whatsapp_channel(receipt, prompt_style="reactions")
    assert isinstance(channel, InteractiveChannel)
    source = whatsapp_messages(whatsapp_event(), phone_number_id="123")[0]
    app = AgentChat(channels=[channel])
    asked = asyncio.create_task(app.ask(channel, source, "Delete build/?", CHOICES))
    try:
        async with TestClient(TestServer(channel.create_app())) as client:
            await until(lambda: receipt.calls)
            body = receipt.calls[0][1]["json"]
            assert body["type"] == "text"
            assert body["text"]["body"] == "Delete build/?\n\nReact with:\n👍 Approve\n👎 Deny"
            # Someone else's message, or an emoji that names no choice, answers nothing.
            for payload in (reaction("👎", "wamid.other", reaction_id="r1"),
                            reaction("🎉", "wamid.prompt", reaction_id="r2")):
                response = await client.post("/webhooks/whatsapp", **signed(payload))
                assert response.status == 200
            await asyncio.sleep(0.01)
            assert not asked.done()
            response = await client.post(
                "/webhooks/whatsapp", **signed(reaction("👎🏽", "wamid.prompt", reaction_id="r3")),
            )
            assert response.status == 200
            answer = await asyncio.wait_for(asked, 1)
            assert (answer.choice, answer.sender.id) == ("no", "15551234567")
            assert channel._reaction_prompts == {}
            assert len(receipt.calls) == 1
    finally:
        await app.close()


def test_whatsapp_prompt_style_is_checked():
    with pytest.raises(ValueError, match="prompt_style"):
        whatsapp_channel(Receipt(), prompt_style="emoji")


@pytest.mark.asyncio
@pytest.mark.parametrize(("choices", "kind"), [
    (CHOICES, "button"),
    ([Choice("a", "A label longer than twenty"), Choice("b", "B")], "list"),
])
async def test_whatsapp_buttons_or_lists_round_trip_by_default(choices, kind):
    receipt = Receipt("wamid.prompt")
    channel = whatsapp_channel(receipt)
    source = whatsapp_messages(whatsapp_event(), phone_number_id="123")[0]
    app = AgentChat(channels=[channel])
    asked = asyncio.create_task(app.ask(channel, source, "Delete build/?", choices))
    try:
        await until(lambda: receipt.calls)
        interactive = receipt.calls[0][1]["json"]["interactive"]
        assert interactive["type"] == kind
        if kind == "button":
            token = interactive["action"]["buttons"][0]["reply"]["id"]
            reply = {"type": "button_reply", "button_reply": {"id": token, "title": "Approve"}}
        else:
            token = interactive["action"]["sections"][0]["rows"][0]["id"]
            reply = {"type": "list_reply", "list_reply": {"id": token, "title": "A label"}}
        press = whatsapp_messages(whatsapp_event(messages=[{
            "id": "wamid.press", "from": "15551234567", "type": "interactive",
            "interactive": reply,
        }]), phone_number_id="123")[0]
        await app._receive(channel, press)
        answer = await asyncio.wait_for(asked, 1)
        assert answer.choice == choices[0].id
        assert len(receipt.calls) == 1
    finally:
        await app.close()


@pytest.mark.asyncio
async def test_ycloud_buttons_round_trip():
    receipt = Receipt("wamid.prompt")
    channel = YCloud(phone_number="+15550001111", api_key="fake", webhook_secret="secret",
                     port=0, **OPTIONS)
    channel._session = YCloudSession(receipt)
    assert isinstance(channel, InteractiveChannel)
    source = ycloud_messages(ycloud_event(), phone_number="+15550001111")[0]
    app = AgentChat(channels=[channel])
    asked = asyncio.create_task(app.ask(channel, source, "Delete build/?", CHOICES))
    try:
        await until(lambda: receipt.calls)
        args, kwargs = receipt.calls[0]
        assert args[0].endswith("/whatsapp/messages/sendDirectly")
        body = kwargs["json"]
        assert (body["from"], body["to"], body["type"]) == (
            "+15550001111", "+15551234567", "interactive",
        )
        token = body["interactive"]["action"]["buttons"][1]["reply"]["id"]
        press = ycloud_messages(ycloud_event(ycloud_message(
            wamid="wamid.press", type="interactive",
            interactive={"type": "button_reply", "button_reply": {"id": token, "title": "Deny"}},
        )), phone_number="+15550001111")[0]
        await app._receive(channel, press)
        answer = await asyncio.wait_for(asked, 1)
        assert (answer.choice, answer.label) == ("no", "Deny")
        assert len(receipt.calls) == 1
    finally:
        await app.close()
