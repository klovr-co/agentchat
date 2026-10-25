"""WhatsApp and YCloud webhook items beyond chat messages, Flows and notices."""

import asyncio
import hashlib
import hmac
import json
import time

import pytest
from aiohttp.test_utils import TestClient, TestServer

from agentchat import AgentChat, RichReply
from agentchat.channels.whatsapp import WhatsApp
from agentchat.channels.whatsapp.events import normalize
from agentchat.channels.ycloud import YCloud
from tests.support.receipts import Receipt
from tests.support.whatsapp import WhatsAppSession, whatsapp_event
from tests.support.ycloud import BUSINESS, YCloudSession, ycloud_event, ycloud_message


def whatsapp():
    return WhatsApp(phone_number_id="123", access_token="token", app_secret="secret",
                    verify_token="verify")


def signed(payload):
    raw = json.dumps(payload).encode()
    signature = hmac.new(b"secret", raw, hashlib.sha256).hexdigest()
    return {"data": raw, "headers": {"X-Hub-Signature-256": f"sha256={signature}"}}


def ycloud_signed(payload):
    raw = json.dumps(payload).encode()
    timestamp = int(time.time())
    signature = hmac.new(b"whsec_secret", f"{timestamp}.".encode() + raw,
                         hashlib.sha256).hexdigest()
    return {"data": raw, "headers": {"YCloud-Signature": f"t={timestamp},s={signature}"}}


async def until(predicate):
    for _ in range(400):
        if predicate():
            return
        await asyncio.sleep(0.005)
    raise AssertionError("Condition was not reached")


def flow_reply(token):
    return {"id": "wamid.flow", "from": "15551234567", "type": "interactive", "interactive": {
        "type": "nfm_reply", "nfm_reply": {"name": "flow", "body": "Sent",
                                          "response_json": json.dumps({"flow_token": token})}}}


@pytest.mark.asyncio
async def test_whatsapp_presses_flows_commands_and_statuses_reach_handlers():
    channel = whatsapp()
    seen, messages = [], []

    async def receive(channel, message):
        messages.append(message.text)

    channel.bind(receive)

    @channel.on_action("retry")
    async def retry(payload):
        seen.append(("action", payload["messages"][0]["id"]))

    @channel.on_view("signup")
    async def signed_up(payload):
        seen.append(("view", payload["messages"][0]["id"]))

    @channel.on_command("/start")
    async def start(payload):
        seen.append(("command", payload["messages"][0]["text"]["body"]))

    @channel.on_event("status")
    async def status(payload):
        seen.append(("status", payload["statuses"][0]["status"]))

    payload = whatsapp_event(messages=[
        {"id": "wamid.press", "from": "15551234567", "type": "interactive",
         "interactive": {"type": "button_reply",
                         "button_reply": {"id": "retry", "title": "Retry"}}},
        flow_reply("signup"),
        {"id": "wamid.cmd", "from": "15551234567", "type": "text", "text": {"body": "/start"}},
        {"id": "wamid.text", "from": "15551234567", "type": "text", "text": {"body": "hello"}},
    ])
    payload["entry"][0]["changes"][0]["value"]["statuses"] = [{"id": "wamid.out", "status": "read"}]
    try:
        async with TestClient(TestServer(channel.create_app())) as client:
            response = await client.post("/webhooks/whatsapp", **signed(payload))
            assert response.status == 200
            await until(lambda: len(seen) == 4 and messages)
    finally:
        await channel.close()
    assert sorted(seen) == [("action", "wamid.press"), ("command", "/start"),
                            ("status", "read"), ("view", "wamid.flow")]
    assert messages == ["hello"]


@pytest.mark.asyncio
async def test_whatsapp_sends_flows_as_rich_replies_and_notices_as_replies(monkeypatch):
    receipt = Receipt("wamid.sent")
    session = WhatsAppSession(receipt)
    monkeypatch.setattr("agentchat.channels.whatsapp.adapter.aiohttp.ClientSession",
                        lambda **kw: session)
    channel = whatsapp()
    app = AgentChat(channels=[channel])
    source = normalize(whatsapp_event(), phone_number_id="123")[0]
    flow = {"type": "flow", "body": {"text": "Sign up"}, "action": {"name": "flow", "parameters": {
        "flow_message_version": "3", "flow_token": "signup", "flow_id": "1",
        "flow_cta": "Open"}}}
    try:
        sent = await app.reply_rich(channel, source, RichReply(text="Sign up", blocks=(flow,)))
        assert sent.text == "Sign up"
        request = receipt.calls[-1][1]["json"]
        assert request["type"] == "interactive" and request["interactive"] == flow
        with pytest.raises(ValueError):
            await app.reply_rich(channel, source, RichReply(text="x", blocks=()))
        notice = await app.send_ephemeral(channel, source, "15551234567", "Only you")
        assert notice.text == "Only you"
        with pytest.raises(ValueError):
            await app.send_ephemeral(channel, source, "15550000000", "Not yours")
        with pytest.raises(TypeError, match="deleting"):
            await app.delete(channel, sent)
    finally:
        await channel.close()


@pytest.mark.asyncio
async def test_ycloud_presses_and_events_reach_handlers(monkeypatch):
    session = YCloudSession(Receipt("wamid.sent"))
    monkeypatch.setattr("agentchat.channels.ycloud.adapter.aiohttp.ClientSession",
                        lambda **kw: session)
    channel = YCloud(phone_number=BUSINESS, api_key="key", webhook_secret="whsec_secret")
    seen, messages = [], []

    async def receive(channel, message):
        messages.append(message.text)

    channel.bind(receive)

    @channel.on_action("retry")
    async def retry(payload):
        seen.append(payload["type"])

    @channel.on_event("whatsapp.message.updated")
    async def updated(payload):
        seen.append(payload["type"])

    press = ycloud_event(ycloud_message(type="interactive", interactive={
        "type": "button_reply", "button_reply": {"id": "retry", "title": "Retry"}}))
    update = {"id": "evt_2", "type": "whatsapp.message.updated", "whatsappMessage": {
        "id": "m", "wamid": "w", "status": "read", "from": BUSINESS, "to": "+15551234567"}}
    try:
        async with TestClient(TestServer(channel.create_app())) as client:
            for payload in (press, update, ycloud_event()):
                response = await client.post("/webhooks/ycloud", **ycloud_signed(payload))
                assert response.status == 200
            await until(lambda: len(seen) == 2 and messages)
    finally:
        await channel.close()
    assert sorted(seen) == ["whatsapp.inbound_message.received", "whatsapp.message.updated"]
    assert messages == ["hello"]
