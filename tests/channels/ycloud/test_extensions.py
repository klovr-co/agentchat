"""Status events, media download, templates, and Business App echoes against simulated YCloud."""
import asyncio
import hashlib
import logging

import pytest
import pytest_asyncio
from aiohttp.test_utils import TestClient, TestServer

from agentchat import AgentChat
from agentchat.channels.ycloud import YCloud, YCloudError, YCloudStatus, YCloudTemplate
from agentchat.channels.ycloud.events import normalize
from agentchat.channels.ycloud.rendering import template_payload
from tests.support.ycloud import (
    API_KEY,
    BUSINESS,
    CUSTOMER,
    SECRET,
    FakeYCloud,
    echo_event,
    signed,
    status_event,
    template,
    ycloud_event,
    ycloud_message,
)


@pytest_asyncio.fixture
async def fake(monkeypatch):
    fake = FakeYCloud()
    async with TestServer(fake.app()) as server:
        monkeypatch.setattr(
            "agentchat.channels.ycloud.adapter.API_ROOT", str(server.make_url("/v2"))
        )
        fake.root = str(server.make_url("/v2"))
        yield fake


def make_channel(**options):
    return YCloud(phone_number=BUSINESS, api_key=API_KEY, webhook_secret=SECRET, **options)


async def post_events(channel, *payloads):
    statuses = []
    async with TestClient(TestServer(channel.create_app())) as client:
        for payload in payloads:
            response = await client.post("/webhooks/ycloud", **signed(payload))
            statuses.append(response.status)
    return statuses


@pytest.mark.asyncio
async def test_status_events_reach_the_event_handler_never_the_agent_and_stay_unordered():
    channel = make_channel()
    app = AgentChat(channels=[channel])
    turns, events = [], []

    @app.on_message
    async def respond(context):
        turns.append(context.message)

    async def on_event(source, event):
        assert source is channel
        events.append(event)

    channel.bind_events(on_event)
    failed = status_event(
        "failed",
        event_id="evt_failed",
        errorCode="131047",
        errorMessage="Re-engagement message",
        whatsappApiError={
            "code": "131047",
            "message": "(#131047) Re-engagement message",
            "type": "OAuthException",
            "error_data": {"details": "More than 24 hours have passed"},
        },
    )
    try:
        # `delivered` arriving after `failed` and `read` before `delivered` are passed as-is.
        codes = await post_events(
            channel,
            failed,
            status_event("delivered", event_id="evt_delivered"),
            status_event("read", event_id="evt_read"),
            status_event("sent", event_id="evt_sent"),
        )
        assert codes == [200, 200, 200, 200]
        assert not turns
        assert [e.status for e in events] == ["failed", "delivered", "read", "sent"]
        assert all(isinstance(e, YCloudStatus) for e in events)
        first = events[0]
        assert first.event_id == "evt_failed"
        assert (first.message_id, first.wamid, first.external_id) == (
            "ycloud-out", "wamid.sent", "outbox-1",
        )
        assert first.error_code == "131047" and first.error_kind == YCloudError.WINDOW_EXPIRED
        assert first.whatsapp_api_error.type == "OAuthException"
        assert first.whatsapp_api_error.error_data == {"details": "More than 24 hours have passed"}
        assert first.conversation_id == f"ycloud:{BUSINESS}:{CUSTOMER}"
        # Status-specific time first, then updateTime, then the event's createTime.
        assert first.timestamp == "2026-10-21T12:00:05.000Z"
        assert events[1].timestamp == "2026-10-21T12:00:02.000Z"
        assert events[2].timestamp == "2026-10-21T12:00:03.000Z"
        assert events[3].timestamp == "2026-10-21T12:00:01.000Z"
        assert events[1].error_kind is None
    finally:
        await app.close()


@pytest.mark.asyncio
async def test_status_for_another_number_or_without_handler_is_acknowledged_and_dropped():
    channel = make_channel()
    seen = []

    async def on_event(source, event):
        seen.append(event)

    try:
        assert await post_events(channel, status_event("sent")) == [200]
        channel.bind_events(on_event)
        foreign = status_event("sent", **{"from": "+15559999999"})
        assert await post_events(channel, foreign) == [200]
        assert not seen
    finally:
        await channel.close()


@pytest.mark.asyncio
async def test_event_handler_failure_returns_500_for_redelivery_without_logging_secrets(caplog):
    channel = make_channel()

    async def on_event(source, event):
        raise RuntimeError("database down")

    channel.bind_events(on_event)
    caplog.set_level(logging.DEBUG)
    try:
        assert await post_events(channel, status_event("sent")) == [500]
        assert await post_events(channel, echo_event()) == [200]  # echoes are off
    finally:
        await channel.close()
    logs = caplog.text
    assert "YCloud event handler failed (RuntimeError)" in logs
    for secret in (API_KEY, SECRET, "private reply text", "database down"):
        assert secret not in logs


@pytest.mark.asyncio
async def test_unsigned_status_and_echo_events_are_rejected_before_parsing():
    channel = make_channel(smb_echoes=True)
    called = []

    async def on_event(source, event):
        called.append(event)

    channel.bind_events(on_event)
    try:
        async with TestClient(TestServer(channel.create_app())) as client:
            for payload in (status_event("read"), echo_event()):
                response = await client.post(
                    "/webhooks/ycloud", **signed(payload, secret="whsec_wrong")
                )
                assert response.status == 401
            response = await client.post(
                "/webhooks/ycloud", data=b"{not json", headers=signed({})["headers"]
            )
            assert response.status == 401
        assert not called
    finally:
        await channel.close()


@pytest.mark.asyncio
async def test_business_app_echoes_are_opt_in_assistant_messages_outside_the_agent():
    events, turns = [], []

    async def on_event(source, event):
        events.append(event)

    off = make_channel()
    off.bind_events(on_event)
    try:
        assert await post_events(off, echo_event()) == [200]
        assert not events
    finally:
        await off.close()

    channel = make_channel(smb_echoes=True)
    app = AgentChat(channels=[channel])

    @app.on_message
    async def respond(context):
        turns.append(context.message)

    channel.bind_events(on_event)
    try:
        assert await post_events(channel, echo_event()) == [200]
        assert not turns
        (echo,) = events
        inbound = normalize(ycloud_event(), phone_number=BUSINESS)[0]
        assert echo.role == "assistant"
        assert echo.conversation_id == inbound.conversation_id
        assert echo.sender.id == BUSINESS
        assert echo.text == "staff typed this in the app"
        assert echo.metadata["echo"] is True and echo.metadata["to"] == CUSTOMER
        assert echo.id == f"ycloud:{BUSINESS}:wamid.echo"
        foreign = echo_event(**{"from": "+15559999999"})
        assert await post_events(channel, foreign) == [200]
        assert len(events) == 1
    finally:
        await app.close()


@pytest.mark.asyncio
async def test_reply_and_template_pass_external_id_and_send_once(fake):
    channel = make_channel()
    source = normalize(ycloud_event(), phone_number=BUSINESS)[0]
    try:
        reply = await channel.reply(source, "thanks", external_id="outbox-7")
        assert fake.sends[0]["externalId"] == "outbox-7"
        assert reply.metadata["external_id"] == "outbox-7"
        assert reply.metadata["message_id"] == "wamid.sent-1"
        sent = await channel.send_template(
            CUSTOMER,
            "order_ready",
            "en_US",
            [{"type": "body", "parameters": [{"type": "text", "text": "A-17"}]}],
            external_id="outbox-8",
        )
        assert fake.sends[1] == {
            "from": BUSINESS,
            "to": CUSTOMER,
            "type": "template",
            "template": {
                "name": "order_ready",
                "language": {"code": "en_US"},
                "components": [
                    {"type": "body", "parameters": [{"type": "text", "text": "A-17"}]}
                ],
            },
            "externalId": "outbox-8",
        }
        assert sent.role == "assistant"
        assert sent.conversation_id == source.conversation_id
        assert sent.metadata["template_name"] == "order_ready"
        assert sent.metadata["ycloud_message_id"] == "ycloud-out-2"
        bare = await channel.send_template(CUSTOMER, "hello_world", "en")
        assert "components" not in fake.sends[2]["template"]
        assert "externalId" not in fake.sends[2]
        assert bare.metadata.get("external_id") is None
        assert len(fake.sends) == 3
    finally:
        await channel.close()


@pytest.mark.parametrize(
    ("status", "code", "whatsapp_code", "kind"),
    [
        (400, "BAD_REQUEST", "131047", YCloudError.WINDOW_EXPIRED),
        (400, "BAD_REQUEST", "132000", YCloudError.TEMPLATE_PARAMETERS),
        (400, "BAD_REQUEST", "132001", YCloudError.TEMPLATE_NOT_FOUND),
        (403, "WHATSAPP_TEMPLATE_UNAVAILABLE", None, YCloudError.TEMPLATE_UNAVAILABLE),
        (403, "BALANCE_INSUFFICIENT", None, YCloudError.BALANCE_INSUFFICIENT),
        (401, "UNAUTHORIZED", None, YCloudError.UNAUTHORIZED),
        (429, "TOO_MANY_REQUESTS", "131056", YCloudError.RATE_LIMITED),
        (500, "INTERNAL_SERVER_ERROR", None, YCloudError.REJECTED),
    ],
)
@pytest.mark.asyncio
async def test_send_errors_are_typed_and_never_retried(fake, status, code, whatsapp_code, kind):
    channel = make_channel()
    source = normalize(ycloud_event(), phone_number=BUSINESS)[0]
    try:
        fake.fail_next_send(status, code, whatsapp_code)
        with pytest.raises(YCloudError) as raised:
            await channel.reply(source, "secret reply body")
        error = raised.value
        assert isinstance(error, RuntimeError)
        assert (error.kind, error.status, error.code) == (kind, status, code)
        assert error.whatsapp_code == whatsapp_code
        assert error.request_id == "req_1"
        assert "secret reply body" not in str(error) and API_KEY not in str(error)
        assert len(fake.sends) == 1
        fake.fail_next_send(status, code, whatsapp_code)
        with pytest.raises(YCloudError) as raised:
            await channel.send_template(CUSTOMER, "order_ready", "en")
        assert raised.value.kind == kind
        assert len(fake.sends) == 2
    finally:
        await channel.close()


@pytest.mark.asyncio
async def test_unreachable_api_is_uncertain_and_not_retried(monkeypatch):
    monkeypatch.setattr("agentchat.channels.ycloud.adapter.API_ROOT", "http://127.0.0.1:9/v2")
    channel = make_channel()
    try:
        with pytest.raises(YCloudError) as raised:
            await channel.send_template(CUSTOMER, "order_ready", "en")
        assert raised.value.kind == YCloudError.UNCERTAIN
    finally:
        await channel.close()


def test_template_payload_validation():
    with pytest.raises(ValueError, match=r"E\.164"):
        template_payload(sender=BUSINESS, recipient="15551234567", name="x", language_code="en")
    with pytest.raises(ValueError, match="lowercase"):
        template_payload(sender=BUSINESS, recipient=CUSTOMER, name="Bad Name", language_code="en")
    with pytest.raises(ValueError, match="list"):
        template_payload(
            sender=BUSINESS, recipient=CUSTOMER, name="x", language_code="en", components={}
        )


@pytest.mark.asyncio
async def test_list_templates_pages_and_filters(fake):
    fake.templates = [template(f"approved_{i:03d}") for i in range(150)]
    fake.templates += [
        template("pending_one", status="PENDING"),
        template("spanish", language="es"),
    ]
    channel = make_channel()
    try:
        templates = await channel.list_templates()
        assert len(templates) == 151
        assert [r["page"] for r in fake.template_requests] == ["1", "2"]
        assert all(r["filter.status"] == "APPROVED" for r in fake.template_requests)
        assert all(r["limit"] == "100" for r in fake.template_requests)
        first = templates[0]
        assert isinstance(first, YCloudTemplate)
        assert (first.name, first.language, first.category, first.status) == (
            "approved_000", "en", "UTILITY", "APPROVED",
        )
        assert first.body == "approved_000 body {{1}}"
        assert [c["type"] for c in first.components] == ["HEADER", "BODY"]
        fake.template_requests.clear()
        spanish = await channel.list_templates(language="es")
        assert [t.name for t in spanish] == ["spanish"]
        assert fake.template_requests[0]["filter.language"] == "es"
        everything = await channel.list_templates(status=None)
        assert len(everything) == 152
    finally:
        await channel.close()


def media_message(fake, media_id, content, chunked=False, **media):
    digest = fake.add_media(media_id, content, chunked=chunked)
    image = {
        "id": media_id,
        "link": f"{fake.root}/whatsapp/media/download/{media_id}?sig=t%3D1%2Cs%3Dabc&payload=x",
        "sha256": digest,
        "mime_type": "image/jpeg",
        **media,
    }
    message = ycloud_message(type="image", text=None, image=image)
    return normalize(ycloud_event(message), phone_number=BUSINESS)[0]


@pytest.mark.asyncio
async def test_media_download_uses_signed_link_then_api_key_once(fake):
    channel = make_channel()
    content = b"\xff\xd8 jpeg bytes" * 100
    source = media_message(fake, "m1", content, filename="photo.jpg")
    try:
        assert source.metadata["media"]["sha256"]
        file = await channel.download_media(source)
        assert (file.content, file.name, file.media_type, file.id) == (
            content, "photo.jpg", "image/jpeg", "m1",
        )
        assert fake.media_requests == [{"id": "m1", "with_key": False, "sig": "t=1,s=abc"}]
        fake.link_fresh = False
        fake.media_requests.clear()
        file = await channel.download_media(dict(source.metadata["media"]))
        assert file.content == content
        assert [r["with_key"] for r in fake.media_requests] == [False, True]
    finally:
        await channel.close()


@pytest.mark.asyncio
async def test_media_download_enforces_size_digest_and_availability(fake):
    channel = make_channel(max_media_bytes=1024)
    try:
        large = media_message(fake, "big", b"x" * 2048)
        with pytest.raises(YCloudError) as raised:
            await channel.download_media(large)
        assert raised.value.kind == YCloudError.MEDIA_TOO_LARGE
        streamed = media_message(fake, "stream", b"y" * 9000, chunked=True)
        with pytest.raises(YCloudError) as raised:
            await channel.download_media(streamed, max_bytes=5000)
        assert raised.value.kind == YCloudError.MEDIA_TOO_LARGE
        assert (await channel.download_media(streamed, max_bytes=10_000)).content == b"y" * 9000
        tampered = media_message(fake, "tamper", b"z" * 10)
        fake.media["tamper"] = (b"different", "image/jpeg", False)
        with pytest.raises(YCloudError) as raised:
            await channel.download_media(tampered)
        assert raised.value.kind == YCloudError.MEDIA_INTEGRITY
        hex_digest = dict(media_message(fake, "hex", b"ok").metadata["media"])
        hex_digest["sha256"] = hashlib.sha256(b"ok").hexdigest()
        assert (await channel.download_media(hex_digest)).content == b"ok"
        gone = dict(media_message(fake, "gone", b"q").metadata["media"])
        del fake.media["gone"]
        with pytest.raises(YCloudError) as raised:
            await channel.download_media(gone)
        assert (raised.value.kind, raised.value.status) == (YCloudError.MEDIA_UNAVAILABLE, 404)
    finally:
        await channel.close()


@pytest.mark.parametrize(
    "link",
    [
        "http://169.254.169.254/latest/meta-data",
        "https://api.ycloud.com.evil.test/v2/whatsapp/media/download/1",
        "https://evil.test/v2/whatsapp/media/download/1",
        "{root}/whatsapp/templates",
        "file:///etc/passwd",
    ],
)
@pytest.mark.asyncio
async def test_media_download_refuses_links_outside_the_ycloud_api(fake, link):
    channel = make_channel()
    try:
        with pytest.raises(ValueError, match="outside the YCloud API"):
            await channel.download_media({"link": link.format(root=fake.root)})
        assert not fake.media_requests and not fake.template_requests
    finally:
        await channel.close()


def test_production_media_allowlist_is_api_ycloud_com_only():
    from agentchat.channels.ycloud import adapter

    assert adapter.API_ROOT == "https://api.ycloud.com/v2"


@pytest.mark.asyncio
async def test_message_without_media_cannot_be_downloaded():
    channel = make_channel()
    try:
        with pytest.raises(ValueError, match="no YCloud media link"):
            await channel.download_media(normalize(ycloud_event(), phone_number=BUSINESS)[0])
    finally:
        await channel.close()


@pytest.mark.asyncio
async def test_inbound_media_still_reaches_the_agent_with_download_metadata(fake):
    channel = make_channel()
    app = AgentChat(channels=[channel])
    got = asyncio.Event()
    seen = []

    @app.on_message
    async def respond(context):
        seen.append(context.message)
        got.set()

    image = {"id": "m9", "link": f"{fake.root}/whatsapp/media/download/m9?sig=s",
             "sha256": "abc=", "mime_type": "image/png", "caption": "look"}
    try:
        payload = ycloud_event(ycloud_message(type="image", text=None, image=image))
        assert await post_events(channel, payload) == [200]
        await asyncio.wait_for(got.wait(), 2)
        assert seen[0].text == "look"
        assert seen[0].metadata["media"] == {
            "id": "m9", "link": image["link"], "sha256": "abc=", "mime_type": "image/png",
        }
    finally:
        await app.close()
