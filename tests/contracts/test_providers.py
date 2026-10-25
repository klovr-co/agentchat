import asyncio
from dataclasses import replace
from types import SimpleNamespace

import pytest
from aiohttp import web

from agentchat import AgentChat, RichReply, UploadFile
from agentchat.channels.base import (
    FileUploadChannel,
    MirroringChannel,
    RichReplyChannel,
    StatusChannel,
)
from agentchat.channels.discord import Discord
from agentchat.channels.discord.events import normalize as discord_message
from agentchat.channels.imessage import IMessage
from agentchat.channels.imessage.events import normalize as imessage_message
from agentchat.channels.teams import Teams
from agentchat.channels.teams.events import normalize as teams_message
from agentchat.channels.telegram import Telegram
from agentchat.channels.telegram.events import normalize as telegram_message
from agentchat.channels.whatsapp import WhatsApp
from agentchat.channels.whatsapp.events import normalize as whatsapp_messages
from agentchat.channels.ycloud import YCloud
from agentchat.channels.ycloud.events import normalize as ycloud_messages
from tests.support.discord import DiscordClient, discord_event
from tests.support.imessage import BubblesClient, SocketClient, imessage_event
from tests.support.receipts import Receipt
from tests.support.teams import APP_ID, TENANT_ID, teams_adapter, teams_event
from tests.support.telegram import TelegramClient, telegram_update
from tests.support.whatsapp import WhatsAppSession, whatsapp_event
from tests.support.ycloud import YCloudSession, ycloud_event


@pytest.fixture(params=["discord", "teams", "imessage", "whatsapp", "ycloud", "telegram"])
def harness(request):
    receipt = Receipt(101 if request.param in ("discord", "telegram") else "sent")
    options = {"max_pending_events": 2, "shutdown_timeout": 0.01}
    capabilities = set()
    if request.param == "discord":
        client = DiscordClient(receipt)
        channel = Discord(bot_token="fake", client=client, send_client=client, **options)
        source = discord_message(discord_event(), bot_id=99)
        capabilities = {StatusChannel}
    elif request.param == "teams":
        adapter, _, _ = teams_adapter(receipt)
        channel = Teams(
            app_id=APP_ID,
            tenant_id=TENANT_ID,
            app_password="fake",
            adapter=adapter,
            port=0,
            **options,
        )
        source = teams_message(teams_event(), app_id=APP_ID, tenant_id=TENANT_ID)
        capabilities = {StatusChannel}
    elif request.param == "imessage":
        channel = IMessage(
            server_url="http://localhost:1234",
            password="fake",
            instance_id="test",
            client=BubblesClient(receipt),
            socket_client=SocketClient(),
            **options,
        )
        source = imessage_message(imessage_event(), instance_id="test")
        capabilities = {StatusChannel}
    elif request.param == "telegram":
        channel = Telegram(bot_token="99:fake", client=TelegramClient(receipt), **options)
        source = telegram_message(telegram_update(), bot_id=99, bot_username="agentchat_bot")
        capabilities = {StatusChannel, RichReplyChannel, FileUploadChannel}
    elif request.param == "ycloud":
        channel = YCloud(
            phone_number="+15550001111",
            api_key="fake",
            webhook_secret="secret",
            port=0,
            **options,
        )
        channel._session = YCloudSession(receipt)
        source = ycloud_messages(ycloud_event(), phone_number="+15550001111")[0]
        capabilities = {StatusChannel}
    else:
        channel = WhatsApp(
            phone_number_id="123",
            access_token="fake",
            app_secret="secret",
            verify_token="verify",
            port=0,
            **options,
        )
        channel._session = WhatsAppSession(receipt)
        source = whatsapp_messages(whatsapp_event(), phone_number_id="123")[0]
        capabilities = {StatusChannel}
    return SimpleNamespace(
        channel=channel, source=source, receipt=receipt, capabilities=capabilities,
    )


@pytest.mark.asyncio
async def test_confirmed_reply_preserves_conversation_and_dedupes_inbound(harness):
    app = AgentChat(channels=[harness.channel])
    seen = []

    @app.on_message
    async def respond(context):
        seen.append(context.message)
        return "confirmed answer"

    try:
        await app._receive(harness.channel, harness.source)
        await app._receive(harness.channel, harness.source)
        history = await app.state.history(harness.source.conversation_id)
        assert seen == [harness.source]
        assert len(harness.receipt.calls) == 1
        assert len(history) == 2
        reply = history[1]
        assert reply.role == "assistant"
        assert reply.channel == harness.source.channel
        assert reply.thread_id == harness.source.thread_id
        assert reply.conversation_id == harness.source.conversation_id
        assert reply.id != harness.source.id
        assert reply.text == "confirmed answer"
    finally:
        await app.close()


@pytest.mark.asyncio
@pytest.mark.parametrize("failure", ["timeout", "missing_receipt"])
async def test_failed_or_uncertain_send_never_appends_or_retries(harness, failure):
    app = AgentChat(channels=[harness.channel])
    if failure == "timeout":
        harness.receipt.error = TimeoutError("uncertain")
    else:
        harness.receipt.value = None
    try:
        with pytest.raises((TimeoutError, RuntimeError)):
            await app.reply(harness.channel, harness.source, "answer")
        assert len(harness.receipt.calls) == 1
        assert await app.state.history(harness.source.conversation_id) == ()
    finally:
        await app.close()


@pytest.mark.asyncio
async def test_rejects_wrong_provider_and_empty_text_before_send(harness):
    try:
        with pytest.raises(ValueError):
            await harness.channel.reply(replace(harness.source, channel="wrong"), "answer")
        with pytest.raises(ValueError):
            await harness.channel.reply(harness.source, " ")
        assert not harness.receipt.calls
    finally:
        await harness.channel.close()


@pytest.mark.asyncio
async def test_optional_capability_fallbacks(harness):
    app = AgentChat(channels=[harness.channel])
    for protocol in (StatusChannel, RichReplyChannel, FileUploadChannel, MirroringChannel):
        assert isinstance(harness.channel, protocol) == (protocol in harness.capabilities)
    try:
        if StatusChannel not in harness.capabilities:
            assert await app.set_status(harness.channel, harness.source, "working") is False
        if RichReplyChannel not in harness.capabilities:
            with pytest.raises(TypeError, match="rich replies"):
                await app.reply_rich(harness.channel, harness.source, RichReply(text="answer"))
        if FileUploadChannel not in harness.capabilities:
            with pytest.raises(TypeError, match="file uploads"):
                await app.upload_files(
                    harness.channel, harness.source, [UploadFile("a.txt", b"a")],
                )
        with pytest.raises(TypeError, match="mirroring"):
            await app.mirror(harness.channel, harness.source, harness.source)
    finally:
        await app.close()


@pytest.mark.asyncio
async def test_run_requires_bind(harness):
    with pytest.raises(RuntimeError, match="Bind"):
        await harness.channel.run()
    await harness.channel.close()


@pytest.mark.asyncio
async def test_close_rejects_new_sends_but_drains_accepted_replies(harness):
    harness.channel._delivery.shutdown_timeout = 1

    async def receive(channel, message):
        await channel.reply(message, "last answer")

    harness.channel.bind(receive)
    assert harness.channel._delivery.submit(harness.channel, harness.source)
    await harness.channel.close()
    assert len(harness.receipt.calls) == 1
    with pytest.raises(RuntimeError, match="closed"):
        await harness.channel.reply(harness.source, "too late")
    assert len(harness.receipt.calls) == 1


@pytest.mark.asyncio
async def test_direct_binding_backpressure_and_shutdown_cancellation(harness):
    started, cancelled = asyncio.Event(), asyncio.Event()

    async def receive(channel, message):
        assert channel is harness.channel
        started.set()
        try:
            await asyncio.Event().wait()
        finally:
            cancelled.set()

    harness.channel.bind(receive)
    delivery = harness.channel._delivery
    assert delivery.submit(harness.channel, harness.source)
    assert delivery.submit(harness.channel, replace(harness.source, id="second"))
    assert not delivery.submit(harness.channel, replace(harness.source, id="overflow"))
    await asyncio.wait_for(started.wait(), timeout=1)
    await harness.channel.close()
    await harness.channel.close()
    assert cancelled.is_set()
    assert not delivery.tasks
    assert not delivery.submit(harness.channel, harness.source)


@pytest.mark.asyncio
async def test_run_and_close_release_resources(harness, monkeypatch):
    async def receive(channel, message):
        pass

    harness.channel.bind(receive)
    started = asyncio.Event()
    start_site = web.TCPSite.start

    async def start(site):
        await start_site(site)
        started.set()

    monkeypatch.setattr(web.TCPSite, "start", start)
    running = asyncio.create_task(harness.channel.run())
    try:
        async with asyncio.timeout(2):
            if harness.channel.name in ("discord", "telegram"):
                await harness.channel._client.started.wait()
            elif harness.channel.name == "imessage":
                await harness.channel._socket.started.wait()
            else:
                await started.wait()
        await harness.channel.close()
        await asyncio.wait_for(running, timeout=2)
        assert harness.channel._delivery.closed
    finally:
        running.cancel()
        await asyncio.gather(running, return_exceptions=True)
        await harness.channel.close()
