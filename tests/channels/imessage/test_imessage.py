import asyncio
from urllib.parse import parse_qs

import pytest
import socketio
from aiohttp import web
from aiohttp.test_utils import TestServer

from agentchat import AgentChat
from agentchat.channels.imessage import IMessage
from agentchat.channels.imessage.events import normalize
from agentchat.channels.imessage.transport import BlueBubblesClient
from tests.support.imessage import (
    CHAT_GUID,
    BubblesClient,
    SocketClient,
    TypingBubblesClient,
    imessage_event,
)
from tests.support.receipts import Receipt


@pytest.mark.parametrize(
    "changes",
    [
        {"isFromMe": True},
        {"isFromMe": None},
        {"associatedMessageType": 2000},
        {"isSystemMessage": True},
        {"itemType": 1},
        {"handle": None},
        {"chats": []},
        {"chats": [{"guid": "SMS;-;+15551234567"}]},
        {"text": ""},
        {"chats": [None]},
        {"guid": ""},
        {"attachments": 7, "text": ""},
    ],
)
def test_filters_echoes_reactions_system_sms_and_malformed_events(changes):
    assert normalize(imessage_event(**changes), instance_id="mac") is None


def test_attachment_only_and_group_sender_identity():
    event = imessage_event(
        text="\ufffc",
        chats=[{"guid": "iMessage;+;group"}],
        attachments=[
            {
                "guid": "file",
                "transferName": "photo.jpg",
                "mimeType": "image/jpeg",
                "totalBytes": 50,
            }
        ],
    )
    message = normalize(event, instance_id="mac")
    assert message.sender.id == "+15551234567"
    assert message.text == ""
    assert message.attachments[0].size == 50
    assert message.conversation_id != normalize(imessage_event(), instance_id="mac").conversation_id
    assert message.id != normalize(event, instance_id="other-mac").id


@pytest.mark.asyncio
async def test_allowlist_applies_to_receiving_and_sending():
    receipt = Receipt()
    channel = IMessage(
        server_url="http://localhost:1234",
        password="fake",
        instance_id="mac",
        client=BubblesClient(receipt),
        socket_client=SocketClient(),
        allowed_chat_guids=[],
    )
    seen = []

    async def receive(_, message):
        seen.append(message)

    channel.bind(receive)
    await channel._on_message(imessage_event())
    with pytest.raises(ValueError, match="not allowed"):
        await channel.reply(normalize(imessage_event(), instance_id="mac"), "answer")
    await channel.close()
    assert not seen and not receipt.calls


@pytest.mark.asyncio
async def test_real_socketio_and_rest_round_trip():
    server_socket = socketio.AsyncServer(async_mode="aiohttp")
    server_app = web.Application()
    server_socket.attach(server_app)
    sends = []
    password = "local-test &secret"
    connected, sent = asyncio.Event(), asyncio.Event()

    @server_socket.event
    async def connect(sid, environ, auth):
        assert parse_qs(environ["QUERY_STRING"])["password"] == [password]
        connected.set()

    async def ping(request):
        assert request.query["password"] == password
        return web.json_response({"status": 200, "data": "pong"})

    async def send(request):
        assert request.query["password"] == password
        sends.append(await request.json())
        sent.set()
        return web.json_response({"status": 200, "data": {"guid": "sent-guid", "error": 0}})

    server_app.router.add_get("/api/v1/ping", ping)
    server_app.router.add_post("/api/v1/message/text", send)
    async with TestServer(server_app) as server:
        channel = IMessage(
            server_url=str(server.make_url("/")), password=password, instance_id="mac"
        )
        app = AgentChat(channels=[channel])

        @app.on_message
        async def respond(context):
            return "echo: " + context.message.text

        running = asyncio.create_task(app.run())
        try:
            await asyncio.wait_for(connected.wait(), timeout=3)
            await server_socket.emit("new-message", imessage_event())
            await asyncio.wait_for(sent.wait(), timeout=3)
            await channel.close()
            await asyncio.wait_for(running, timeout=3)
            assert sends[0]["message"] == "echo: hello"
            assert sends[0]["chatGuid"] == CHAT_GUID
            assert sends[0]["method"] == "apple-script"
            assert sends[0]["tempGuid"] != "sent-guid"
            source = normalize(imessage_event(), instance_id="mac")
            history = await app.state.history(source.conversation_id)
            assert len(history) == 2
            assert history[-1].id == "imessage:mac:sent-guid"
            assert channel._client._session.closed
            assert not channel._socket.connected
        finally:
            await app.close()
            running.cancel()
            await asyncio.gather(running, return_exceptions=True)
            await server_socket.shutdown()


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "status,data",
    [
        (401, {"message": "contains-secret"}),
        (302, {}),
        (200, {"status": 200, "data": {"tempGuid": "not-a-receipt"}}),
        (200, {"status": 200, "data": {"guid": "sent", "error": 4}}),
    ],
)
async def test_http_errors_redirects_and_unconfirmed_sends(status, data):
    count = 0

    async def send(request):
        nonlocal count
        count += 1
        return web.json_response(data, status=status, headers={"Location": "/never-follow"})

    server_app = web.Application()
    server_app.router.add_post("/api/v1/message/text", send)
    async with TestServer(server_app) as server:
        client = BlueBubblesClient(str(server.make_url("/")), "contains-secret")
        try:
            with pytest.raises(RuntimeError) as error:
                await client.send_text(CHAT_GUID, "hello")
            assert "contains-secret" not in str(error.value)
            assert count == 1
        finally:
            await client.close()


@pytest.mark.parametrize(
    "url",
    [
        "http://example.com",
        "https://user:password@example.com",
        "https://example.com?password=x",
        "https://example.com/path",
        "ftp://example.com",
    ],
)
def test_url_validation(url):
    with pytest.raises(ValueError):
        BlueBubblesClient(url, "secret")


@pytest.mark.asyncio
async def test_typing_is_false_without_the_private_api_or_client_support():
    source = normalize(imessage_event(), instance_id="mac")
    off = IMessage(
        server_url="http://localhost:1234", password="fake", instance_id="mac",
        client=TypingBubblesClient(Receipt(), private_api=False), socket_client=SocketClient(),
    )
    without = IMessage(
        server_url="http://localhost:1234", password="fake", instance_id="mac",
        client=BubblesClient(Receipt()), socket_client=SocketClient(),
    )
    try:
        assert await off.set_status(source, "thinking") is False
        assert await without.set_status(source, "thinking") is False
        assert await without.set_status(source, "") is True
    finally:
        await off.close()
        await without.close()


# What a BlueBubbles server answers when its Private API is off.
PRIVATE_API_OFF = {
    "status": 500,
    "message": "Please make sure you have completed the setup for the Private API, "
    "and your helper is connected!",
    "error": {"type": "iMessage Error", "message": "iMessage Private API is not enabled!"},
}


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "status,body,shown",
    [
        (200, {"status": 200, "message": "Successfully started typing!"}, True),
        (500, PRIVATE_API_OFF, False),
        (404, {"status": 404, "message": "Not Found"}, False),
    ],
)
async def test_bluebubbles_typing_route_and_private_api_check(status, body, shown):
    paths = []

    async def typing(request):
        assert request.query["password"] == "secret"
        paths.append(request.match_info["guid"])
        return web.json_response(body, status=status)

    server_app = web.Application()
    server_app.router.add_post("/api/v1/chat/{guid}/typing", typing)
    async with TestServer(server_app) as server:
        client = BlueBubblesClient(str(server.make_url("/")), "secret")
        try:
            assert await client.start_typing(CHAT_GUID) is shown
            assert paths == [CHAT_GUID]
        finally:
            await client.close()


@pytest.mark.asyncio
async def test_bluebubbles_typing_errors_still_raise():
    async def typing(request):
        return web.json_response({"status": 401, "message": "Unauthorized"}, status=401)

    server_app = web.Application()
    server_app.router.add_post("/api/v1/chat/{guid}/typing", typing)
    async with TestServer(server_app) as server:
        client = BlueBubblesClient(str(server.make_url("/")), "secret")
        try:
            with pytest.raises(RuntimeError, match="HTTP 401"):
                await client.start_typing(CHAT_GUID)
        finally:
            await client.close()


@pytest.mark.asyncio
async def test_clearing_typing_uses_the_socket_event():
    server_socket = socketio.AsyncServer(async_mode="aiohttp")
    server_app = web.Application()
    server_socket.attach(server_app)
    stopped, typed = asyncio.Event(), []

    @server_socket.on("stopped-typing")
    async def stop(sid, data):
        typed.append(("stop", data))
        stopped.set()

    async def ping(request):
        return web.json_response({"status": 200, "data": "pong"})

    async def typing(request):
        typed.append((request.method, request.match_info["guid"]))
        return web.json_response({"status": 200, "message": "Successfully started typing!"})

    server_app.router.add_get("/api/v1/ping", ping)
    server_app.router.add_route("*", "/api/v1/chat/{guid}/typing", typing)
    async with TestServer(server_app) as server:
        channel = IMessage(server_url=str(server.make_url("/")), password="pw", instance_id="mac")
        channel.bind(lambda *_: None)
        running = asyncio.create_task(channel.run())
        try:
            for _ in range(100):
                if channel._socket.connected:
                    break
                await asyncio.sleep(0.02)
            source = normalize(imessage_event(), instance_id="mac")
            assert await channel.set_status(source, "thinking") is True
            assert await channel.set_status(source, "") is True
            await asyncio.wait_for(stopped.wait(), timeout=3)
            # Never DELETE: on current servers that route starts typing again.
            assert typed == [("POST", CHAT_GUID), ("stop", {"chatGuid": CHAT_GUID})]
        finally:
            await channel.close()
            await asyncio.gather(running, return_exceptions=True)
            await server_socket.shutdown()
