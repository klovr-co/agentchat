"""Socket Mode owns acknowledgement, backpressure and connection cleanup."""

import asyncio

import pytest
from slack_sdk.socket_mode.request import SocketModeRequest

from agentchat.channels.slack.socket_mode import SocketMode
from tests.support.slack import channel_event


class FakeSocket:
    def __init__(self):
        self.socket_mode_request_listeners = []
        self.started = asyncio.Event()
        self.closed = asyncio.Event()
        self.ack_started = asyncio.Event()
        self.ack_release = None
        self.ack_error = None
        self.connect_error = None
        self.connected = False
        self.acknowledgements = []
        self.close_calls = 0

    async def connect(self):
        if self.connect_error:
            raise self.connect_error
        self.connected = True
        self.started.set()

    async def is_connected(self):
        return self.connected

    async def close(self):
        self.connected = False
        self.close_calls += 1
        self.closed.set()

    async def send_socket_mode_response(self, response):
        self.acknowledgements.append(response.envelope_id)
        self.ack_started.set()
        if self.ack_release:
            await self.ack_release.wait()
        if self.ack_error:
            raise self.ack_error

    async def emit(self, envelope_id, ts="1.0", request_type="events_api"):
        request = SocketModeRequest(
            type=request_type, envelope_id=envelope_id, payload=channel_event(ts),
        )
        await self.socket_mode_request_listeners[0](self, request)


@pytest.fixture
def socket(monkeypatch):
    client = FakeSocket()
    monkeypatch.setattr(
        "agentchat.channels.slack.socket_mode.SocketModeClient", lambda **kwargs: client,
    )
    return client


async def start_transport(socket, receive, *, max_pending_events=64):
    source = SocketMode(app_token="xapp-test", max_pending_events=max_pending_events)
    run = asyncio.create_task(source.run(receive))
    await asyncio.wait_for(socket.started.wait(), timeout=1)
    return source, run


@pytest.mark.asyncio
async def test_socket_acknowledges_without_waiting_for_handlers_and_drains_on_close(socket):
    started, release = asyncio.Event(), asyncio.Event()
    received = []

    async def receive(payload):
        started.set()
        await release.wait()
        received.append(payload["event_id"])

    source, run = await start_transport(socket, receive, max_pending_events=2)
    try:
        await socket.emit("one")
        await asyncio.wait_for(started.wait(), timeout=1)
        await asyncio.wait_for(socket.emit("two", "2.0"), timeout=1)
        assert socket.acknowledgements == ["one", "two"]
        assert not received
        await socket.emit("overflow", "3.0")
        await socket.emit("unsupported", request_type="hello")
        assert socket.acknowledgements == ["one", "two"]

        closing = asyncio.create_task(source.close())
        await asyncio.wait_for(socket.closed.wait(), timeout=1)
        await socket.emit("after-close", "4.0")
        assert socket.acknowledgements == ["one", "two"]
        release.set()
        await asyncio.wait_for(closing, timeout=1)
        assert received == ["event-1.0", "event-2.0"]
        assert not await source.is_connected()
    finally:
        release.set()
        await source.close()
        await asyncio.wait_for(run, timeout=1)

    assert socket.close_calls == 1


@pytest.mark.asyncio
async def test_socket_reserves_capacity_before_awaiting_acknowledgement(socket):
    socket.ack_release = asyncio.Event()
    processed = asyncio.Event()

    async def receive(payload):
        processed.set()

    source, run = await start_transport(socket, receive, max_pending_events=1)
    first = asyncio.create_task(socket.emit("one"))
    try:
        await asyncio.wait_for(socket.ack_started.wait(), timeout=1)
        await asyncio.wait_for(socket.emit("overflow", "2.0"), timeout=1)
        assert socket.acknowledgements == ["one"]
        assert not processed.is_set()
        socket.ack_release.set()
        await first
        await asyncio.wait_for(processed.wait(), timeout=1)
    finally:
        socket.ack_release.set()
        await first
        await source.close()
        await asyncio.wait_for(run, timeout=1)


@pytest.mark.asyncio
async def test_failed_acknowledgement_releases_capacity_without_running_handler(socket):
    processed = asyncio.Event()
    received = []

    async def receive(payload):
        received.append(payload["event_id"])
        processed.set()

    source, run = await start_transport(socket, receive, max_pending_events=1)
    try:
        socket.ack_error = TimeoutError("acknowledgement failed")
        with pytest.raises(TimeoutError, match="acknowledgement failed"):
            await socket.emit("failed")
        assert not received
        socket.ack_error = None
        await socket.emit("retry")
        await asyncio.wait_for(processed.wait(), timeout=1)
        assert received == ["event-1.0"]
    finally:
        await source.close()
        await asyncio.wait_for(run, timeout=1)


@pytest.mark.asyncio
async def test_listener_cancellation_does_not_discard_an_accepted_event(socket):
    socket.ack_release = asyncio.Event()
    processed = asyncio.Event()

    async def receive(payload):
        processed.set()

    source, run = await start_transport(socket, receive)
    listener = asyncio.create_task(socket.emit("one"))
    try:
        await asyncio.wait_for(socket.ack_started.wait(), timeout=1)
        listener.cancel()
        with pytest.raises(asyncio.CancelledError):
            await listener
        socket.ack_release.set()
        await asyncio.wait_for(processed.wait(), timeout=1)
        assert socket.acknowledgements == ["one"]
    finally:
        socket.ack_release.set()
        await source.close()
        await asyncio.wait_for(run, timeout=1)


@pytest.mark.asyncio
async def test_connection_failure_closes_socket_and_preserves_the_error(socket):
    refusal = ConnectionError("socket unavailable")
    socket.connect_error = refusal
    source = SocketMode(app_token="xapp-test")

    async def receive(payload):
        raise AssertionError("A failed connection cannot deliver events")

    with pytest.raises(ConnectionError) as caught:
        await source.run(receive)
    assert caught.value is refusal
    assert socket.close_calls == 1
    assert not await source.is_connected()
    await source.close()
    assert socket.close_calls == 1


@pytest.mark.asyncio
async def test_close_during_connection_startup_cancels_connection(socket, monkeypatch):
    connecting, cancelled = asyncio.Event(), asyncio.Event()

    async def connect():
        connecting.set()
        try:
            await asyncio.Event().wait()
        finally:
            cancelled.set()

    monkeypatch.setattr(socket, "connect", connect)
    source = SocketMode(app_token="xapp-test")

    async def receive(payload):
        raise AssertionError("A closed connection cannot deliver events")

    run = asyncio.create_task(source.run(receive))
    try:
        await asyncio.wait_for(connecting.wait(), timeout=1)
        await asyncio.wait_for(source.close(), timeout=1)
        await asyncio.wait_for(run, timeout=1)
        assert cancelled.is_set()
        assert not await source.is_connected()
        assert socket.close_calls == 1
    finally:
        await source.close()
        await asyncio.wait_for(run, timeout=1)


@pytest.mark.asyncio
async def test_run_cancellation_closes_connection_and_cancels_unfinished_handlers(
    socket, monkeypatch,
):
    monkeypatch.setattr("agentchat.channels.slack.socket_mode.EVENT_DRAIN_TIMEOUT", 0)
    started, cancelled = asyncio.Event(), asyncio.Event()

    async def receive(payload):
        started.set()
        try:
            await asyncio.Event().wait()
        finally:
            cancelled.set()

    source, run = await start_transport(socket, receive)
    await socket.emit("one")
    await asyncio.wait_for(started.wait(), timeout=1)
    run.cancel()
    with pytest.raises(asyncio.CancelledError):
        await asyncio.wait_for(run, timeout=1)
    assert cancelled.is_set()
    assert socket.close_calls == 1
    assert not await source.is_connected()
    await source.close()
    assert socket.close_calls == 1


@pytest.mark.parametrize("capacity", [0, -1])
def test_socket_rejects_nonpositive_pending_capacity(capacity):
    with pytest.raises(ValueError, match="positive"):
        SocketMode(app_token="xapp-test", max_pending_events=capacity)
