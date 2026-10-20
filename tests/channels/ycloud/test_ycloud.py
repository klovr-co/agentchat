import asyncio
import hashlib
import hmac
import json
import time

import pytest
from aiohttp import web
from aiohttp.test_utils import TestClient, TestServer

from agentchat import AgentChat
from agentchat.channels.ycloud import YCloud
from agentchat.channels.ycloud.events import normalize
from agentchat.channels.ycloud.rendering import text_payload
from tests.support.receipts import Receipt
from tests.support.ycloud import BUSINESS, CUSTOMER, YCloudSession, ycloud_event, ycloud_message


def make_channel(**options):
    return YCloud(phone_number=BUSINESS, api_key="key", webhook_secret="whsec_secret", **options)


def signed(payload, timestamp=None):
    raw = json.dumps(payload).encode()
    timestamp = int(time.time()) if timestamp is None else timestamp
    signature = hmac.new(
        b"whsec_secret", f"{timestamp}.".encode() + raw, hashlib.sha256
    ).hexdigest()
    return {"data": raw, "headers": {"YCloud-Signature": f"t={timestamp},s={signature}"}}


def test_configuration_requires_e164_business_number():
    with pytest.raises(ValueError, match=r"E\.164"):
        YCloud(phone_number="15550001111", api_key="key", webhook_secret="secret")
    with pytest.raises(ValueError, match="required"):
        YCloud(phone_number=BUSINESS, api_key="", webhook_secret="secret")


@pytest.mark.asyncio
async def test_rejects_unsigned_tampered_and_stale_webhooks():
    channel = make_channel()
    try:
        async with TestClient(TestServer(channel.create_app())) as client:
            response = await client.post("/webhooks/ycloud", json=ycloud_event())
            assert response.status == 401
            payload = signed(ycloud_event())
            payload["data"] += b" "
            response = await client.post("/webhooks/ycloud", **payload)
            assert response.status == 401
            stale = signed(ycloud_event(), timestamp=int(time.time()) - 600)
            response = await client.post("/webhooks/ycloud", **stale)
            assert response.status == 401
    finally:
        await channel.close()


@pytest.mark.asyncio
async def test_real_webhook_to_api_round_trip_and_duplicate_claim(monkeypatch):
    sends = []
    sent = asyncio.Event()

    async def send(request):
        assert request.headers["X-API-Key"] == "key"
        sends.append(await request.json())
        sent.set()
        return web.json_response({"id": "ycloud-out", "wamid": "wamid.sent", "status": "sent"})

    api = web.Application()
    api.router.add_post("/v2/whatsapp/messages/sendDirectly", send)
    async with TestServer(api) as api_server:
        monkeypatch.setattr(
            "agentchat.channels.ycloud.adapter.API_ROOT",
            str(api_server.make_url("/v2")),
        )
        channel = make_channel()
        app = AgentChat(channels=[channel])

        @app.on_message
        async def respond(context):
            return "echo: " + context.message.text

        try:
            async with TestClient(TestServer(channel.create_app())) as client:
                response = await client.post("/webhooks/ycloud", **signed(ycloud_event()))
                assert response.status == 200
                await asyncio.wait_for(sent.wait(), timeout=2)
                response = await client.post("/webhooks/ycloud", **signed(ycloud_event()))
                assert response.status == 200
                await app.close()
                assert sends == [
                    {
                        "from": BUSINESS,
                        "to": CUSTOMER,
                        "type": "text",
                        "text": {"body": "echo: hello", "preview_url": False},
                        "context": {"message_id": "wamid.incoming"},
                    }
                ]
                history = await app.state.history(f"ycloud:{BUSINESS}:{CUSTOMER}")
                assert history[-1].id == f"ycloud:{BUSINESS}:wamid.sent"
                assert history[-1].metadata["ycloud_message_id"] == "ycloud-out"
        finally:
            await app.close()


@pytest.mark.asyncio
async def test_overload_returns_503_without_accepting():
    channel = make_channel(max_pending_events=1, shutdown_timeout=0.01)
    seen = []
    release = asyncio.Event()

    async def receive(channel, source):
        seen.append(source)
        await release.wait()

    channel.bind(receive)
    channel._delivery.submit(channel, normalize(ycloud_event(), phone_number=BUSINESS)[0])
    try:
        async with TestClient(TestServer(channel.create_app())) as client:
            event = ycloud_event(ycloud_message(wamid="wamid.second"))
            response = await client.post("/webhooks/ycloud", **signed(event))
            assert response.status == 503
            await asyncio.sleep(0)
            assert [m.metadata["message_id"] for m in seen] == ["wamid.incoming"]
            assert len(channel._delivery.tasks) == 1
    finally:
        release.set()
        await channel.close()


def test_media_button_inputs_and_number_isolation():
    image = normalize(
        ycloud_event(
            ycloud_message(
                type="image",
                text=None,
                image={"id": "media-id", "link": "https://media", "mime_type": "image/png"},
            )
        ),
        phone_number=BUSINESS,
    )
    assert image[0].attachments[0].id == "media-id"
    assert image[0].attachments[0].media_type == "image/png"
    link_only = normalize(
        ycloud_event(ycloud_message(type="document", document={"link": "https://doc"})),
        phone_number=BUSINESS,
    )
    assert link_only[0].attachments[0].id == "https://doc"
    reply = normalize(
        ycloud_event(
            ycloud_message(
                type="interactive",
                customerProfile=None,
                interactive={"type": "button_reply", "button_reply": {"id": "y", "title": "Yes"}},
            )
        ),
        phone_number=BUSINESS,
    )[0]
    assert reply.text == "Yes" and reply.sender.display_name is None
    message = normalize(ycloud_event(), phone_number=BUSINESS)[0]
    assert message.id == f"ycloud:{BUSINESS}:wamid.incoming"
    assert message.conversation_id == f"ycloud:{BUSINESS}:{CUSTOMER}"
    assert message.sender.display_name == "Alice"
    assert message.metadata["ycloud_message_id"] == "ycloud-in"
    assert normalize(ycloud_event(), phone_number="+15559999999") == ()


@pytest.mark.parametrize(
    "payload",
    [
        None,
        [],
        {"type": "whatsapp.message.updated", "whatsappMessage": {"id": "x"}},
        ycloud_event(ycloud_message(type="reaction", reaction={"emoji": "👍"})),
        ycloud_event(ycloud_message(groupId="group")),
        ycloud_event(ycloud_message(**{"from": "15551234567"})),
        ycloud_event(ycloud_message(wamid="")),
    ],
)
def test_malformed_status_group_and_unsupported_events_are_ignored(payload):
    assert normalize(payload, phone_number=BUSINESS) == ()


def test_ycloud_text_limit():
    payload = text_payload("a" * 4096, sender=BUSINESS, recipient=CUSTOMER, message_id="m")
    assert payload["type"] == "text" and payload["from"] == BUSINESS
    with pytest.raises(ValueError, match="4,096"):
        text_payload("a" * 4097, sender=BUSINESS, recipient=CUSTOMER, message_id="m")


@pytest.mark.asyncio
async def test_reply_rejects_foreign_sources_and_preserves_host_metadata(monkeypatch):
    from dataclasses import replace

    from agentchat.channels.whatsapp.events import normalize as whatsapp_normalize
    from tests.support.whatsapp import whatsapp_event

    marker = object()
    source = normalize(ycloud_event(), phone_number=BUSINESS)[0]
    source = replace(source, metadata={**source.metadata, "host_marker": marker})
    session = YCloudSession(Receipt("wamid.sent"))
    monkeypatch.setattr(
        "agentchat.channels.ycloud.adapter.aiohttp.ClientSession", lambda **kw: session,
    )
    channel = make_channel()
    try:
        reply = await channel.reply(source, "native receipt")
        assert reply.metadata["host_marker"] is marker
        assert reply.metadata["message_id"] == "wamid.sent"
        foreign = whatsapp_normalize(whatsapp_event(), phone_number_id="123")[0]
        with pytest.raises(ValueError, match="YCloud adapter"):
            await channel.reply(foreign, "nope")
    finally:
        await channel.close()
    assert session.closed
