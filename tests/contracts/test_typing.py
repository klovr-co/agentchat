"""Typing indicators for providers that clear them on their own after a few seconds."""
import asyncio
from dataclasses import replace
from types import SimpleNamespace

import pytest

from agentchat import AgentChat
from agentchat.channels.discord import Discord
from agentchat.channels.discord import adapter as discord_adapter
from agentchat.channels.discord.events import normalize as discord_message
from agentchat.channels.imessage import IMessage
from agentchat.channels.imessage import adapter as imessage_adapter
from agentchat.channels.imessage.events import normalize as imessage_message
from agentchat.channels.teams import Teams
from agentchat.channels.teams import adapter as teams_adapter_module
from agentchat.channels.teams.events import normalize as teams_message
from agentchat.channels.whatsapp import WhatsApp
from agentchat.channels.whatsapp import adapter as whatsapp_adapter
from agentchat.channels.whatsapp.events import normalize as whatsapp_messages
from agentchat.channels.ycloud import YCloud
from agentchat.channels.ycloud import adapter as ycloud_adapter
from agentchat.channels.ycloud.events import normalize as ycloud_messages
from tests.support.discord import DiscordClient, discord_event
from tests.support.imessage import CHAT_GUID, SocketClient, TypingBubblesClient, imessage_event
from tests.support.receipts import Receipt
from tests.support.teams import APP_ID, TENANT_ID, teams_adapter, teams_event
from tests.support.telegram import until
from tests.support.whatsapp import WhatsAppSession, whatsapp_event
from tests.support.ycloud import YCloudSession, ycloud_event

OPTIONS = {"max_pending_events": 2, "shutdown_timeout": 0.01}


def discord(receipt):
    client = DiscordClient(receipt)
    channel = Discord(bot_token="fake", client=client, send_client=client, **OPTIONS)

    def fail(error):
        client.typing_error = error

    return SimpleNamespace(
        channel=channel,
        source=discord_message(discord_event(), bot_id=99),
        sent=lambda: list(client.typing),
        first=10,
        fail=fail,
    )


def teams(receipt):
    adapter, _, _ = teams_adapter(receipt)
    channel = Teams(
        app_id=APP_ID, tenant_id=TENANT_ID, app_password="fake", adapter=adapter, port=0, **OPTIONS
    )

    def sent():
        activities = [args[-1] for args, _ in receipt.calls]
        return [(a.type, a.conversation.id) for a in activities if a.type == "typing"]

    return SimpleNamespace(
        channel=channel,
        source=teams_message(teams_event(), app_id=APP_ID, tenant_id=TENANT_ID),
        sent=sent,
        first=("typing", "conversation"),
        fail=lambda error: setattr(receipt, "error", error),
    )


def whatsapp(receipt):
    channel = WhatsApp(
        phone_number_id="123",
        access_token="fake",
        app_secret="secret",
        verify_token="verify",
        port=0,
        **OPTIONS,
    )
    channel._session = WhatsAppSession(receipt)

    def sent():
        bodies = [kwargs["json"] for _, kwargs in receipt.calls]
        return [body for body in bodies if "typing_indicator" in body]

    return SimpleNamespace(
        channel=channel,
        source=whatsapp_messages(whatsapp_event(), phone_number_id="123")[0],
        sent=sent,
        first={
            "messaging_product": "whatsapp",
            "status": "read",
            "message_id": "wamid.incoming",
            "typing_indicator": {"type": "text"},
        },
        fail=lambda error: setattr(receipt, "error", error),
    )


def ycloud(receipt):
    channel = YCloud(
        phone_number="+15550001111", api_key="fake", webhook_secret="secret", port=0, **OPTIONS,
    )
    channel._session = YCloudSession(receipt)

    def sent():
        return [args[0] for args, _ in receipt.calls if "/typing" in args[0]]

    return SimpleNamespace(
        channel=channel,
        source=ycloud_messages(ycloud_event(), phone_number="+15550001111")[0],
        sent=sent,
        first="https://api.ycloud.com/v2/whatsapp/inboundMessages/wamid.incoming/typing",
        fail=lambda error: setattr(receipt, "error", error),
    )


def imessage(receipt):
    client = TypingBubblesClient(receipt)
    channel = IMessage(
        server_url="http://localhost:1234", password="fake", instance_id="mac",
        client=client, socket_client=SocketClient(), **OPTIONS,
    )
    return SimpleNamespace(
        channel=channel,
        source=imessage_message(imessage_event(), instance_id="mac"),
        sent=lambda: list(client.typed),
        first=CHAT_GUID,
        fail=lambda error: setattr(receipt, "error", error),
    )


PROVIDERS = {
    "discord": (discord, discord_adapter),
    "imessage": (imessage, imessage_adapter),
    "teams": (teams, teams_adapter_module),
    "whatsapp": (whatsapp, whatsapp_adapter),
    "ycloud": (ycloud, ycloud_adapter),
}


@pytest.fixture(params=sorted(PROVIDERS))
def typing(request, monkeypatch):
    build, module = PROVIDERS[request.param]
    monkeypatch.setattr(module, "TYPING_INTERVAL", 0.01)
    monkeypatch.setattr(module, "TYPING_EXPIRY", 0.2)
    receipt = Receipt(101 if request.param == "discord" else "sent")
    harness = build(receipt)
    harness.receipt = receipt
    return harness


async def settled(sent):
    """Wait long enough for a running refresh to have sent again, then count."""
    before = len(sent())
    await asyncio.sleep(0.05)
    return before, len(sent())


@pytest.mark.asyncio
async def test_typing_refreshes_until_cleared(typing):
    try:
        assert await typing.channel.set_status(typing.source, "thinking") is True
        assert typing.sent()[0] == typing.first
        await until(lambda: len(typing.sent()) >= 3)
        assert await typing.channel.set_status(typing.source, "") is True
        before, after = await settled(typing.sent)
        assert before == after
    finally:
        await typing.channel.close()


@pytest.mark.asyncio
async def test_a_new_status_replaces_the_refresh_instead_of_adding_one(typing):
    try:
        await typing.channel.set_status(typing.source, "reading")
        await typing.channel.set_status(typing.source, "writing")
        await asyncio.sleep(0.1)
        # One refresh sends about every 0.01s; two would roughly double that.
        assert len(typing.sent()) < 15
    finally:
        await typing.channel.close()


@pytest.mark.asyncio
async def test_typing_expires_and_later_failures_are_swallowed(typing):
    try:
        await typing.channel.set_status(typing.source, "thinking")
        await until(lambda: len(typing.sent()) >= 2)
        typing.fail(TimeoutError("busy"))
        await asyncio.sleep(0.3)
        before, after = await settled(typing.sent)
        assert before == after
        with pytest.raises((TimeoutError, RuntimeError)):
            await typing.channel.set_status(typing.source, "thinking")
    finally:
        await typing.channel.close()


@pytest.mark.asyncio
async def test_close_stops_typing_and_later_status_is_refused(typing):
    await typing.channel.set_status(typing.source, "thinking")
    await typing.channel.close()
    before, after = await settled(typing.sent)
    assert before == after
    with pytest.raises(RuntimeError, match="closed"):
        await typing.channel.set_status(typing.source, "thinking")


@pytest.mark.asyncio
async def test_foreign_source_is_rejected_before_any_request(typing):
    try:
        with pytest.raises(ValueError):
            await typing.channel.set_status(replace(typing.source, channel="wrong"), "thinking")
        assert typing.sent() == []
    finally:
        await typing.channel.close()


@pytest.mark.asyncio
async def test_working_keeps_typing_for_the_handler_and_clears_it(typing):
    app = AgentChat(channels=[typing.channel])
    try:
        async with app.working(typing.channel, typing.source, "thinking"):
            await until(lambda: len(typing.sent()) >= 3)
        before, after = await settled(typing.sent)
        assert before == after
    finally:
        await app.close()


@pytest.mark.asyncio
async def test_discord_send_client_without_typing_reports_unsupported():
    class SendOnly:
        async def send_text(self, channel_id, payload):
            return 101

        async def close(self):
            pass

    client = DiscordClient(Receipt(101))
    channel = Discord(bot_token="fake", client=client, send_client=SendOnly(), **OPTIONS)
    try:
        source = discord_message(discord_event(), bot_id=99)
        assert await channel.set_status(source, "thinking") is False
        assert await channel.set_status(source, "") is True
    finally:
        await channel.close()
