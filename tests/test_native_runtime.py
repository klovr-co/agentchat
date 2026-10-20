"""Compatibility at the native boundary, including host task and object identity."""
import asyncio
import contextvars
import gc
import weakref
from contextlib import asynccontextmanager
from dataclasses import replace
from types import MappingProxyType

import pytest

from agentchat import AgentChat
from agentchat._native import NativeTurn, core_version
from agentchat.state import MemoryState
from tests.support.channels import FakeChannel, incoming


@pytest.mark.asyncio
@pytest.mark.parametrize("backend", ["rust"])
async def test_custom_state_scope_and_nested_calls_stay_in_the_original_task(backend):
    scope = contextvars.ContextVar("conversation_scope", default=None)
    caller = asyncio.current_task()
    events = []
    opaque = object()

    class HostState(MemoryState):
        @asynccontextmanager
        async def lock(self, conversation_id):
            assert asyncio.current_task() is caller
            token = scope.set(conversation_id)
            events.append("enter")
            try:
                async with super().lock(conversation_id):
                    yield
            finally:
                assert asyncio.current_task() is caller
                scope.reset(token)
                events.append("exit")

    channel = FakeChannel()
    state = HostState()
    app = AgentChat(channels=[channel], state=state, backend=backend)
    source = replace(incoming("one", "hello"), metadata=MappingProxyType({"opaque": opaque}))

    @app.on_message
    async def handler(context):
        assert asyncio.current_task() is caller
        assert scope.get() == source.conversation_id
        assert context.message is source
        assert context.message.metadata["opaque"] is opaque
        assert (await context.history())[0] is source
        await context.reply("explicit")
        return "automatic"

    assert handler is app._handler
    # Unlike wait_for, timeout preserves the current host task.
    async with asyncio.timeout(2):
        await channel.send(source)
    assert events == ["enter", "exit"]
    assert scope.get() is None
    assert channel.replies == ["explicit", "automatic"]
    assert [m.text for m in await state.history(source.conversation_id)] == [
        "hello", "explicit", "automatic",
    ]


@pytest.mark.asyncio
@pytest.mark.parametrize("backend", ["rust"])
async def test_handler_failure_preserves_exception_and_claim_without_replay(backend):
    channel = FakeChannel()
    app = AgentChat(channels=[channel], backend=backend)
    original = LookupError("user failure")
    calls = 0

    @app.on_message
    async def handler(context):
        nonlocal calls
        calls += 1
        raise original

    source = incoming("failed", "hello")
    with pytest.raises(LookupError) as failure:
        await channel.send(source)
    assert failure.value is original
    await channel.send(source)
    assert calls == 1
    assert channel.replies == []
    assert await app.state.history(source.conversation_id) == (source,)


@pytest.mark.asyncio
@pytest.mark.parametrize("backend", ["rust"])
async def test_cancelled_turn_releases_custom_lock_and_retains_claim(backend):
    channel = FakeChannel()
    app = AgentChat(channels=[channel], backend=backend)
    started = asyncio.Event()

    @app.on_message
    async def handler(context):
        if context.message.id == "cancelled":
            started.set()
            await asyncio.Event().wait()
        return "next"

    source = incoming("cancelled", "hello")
    task = asyncio.create_task(channel.send(source))
    await asyncio.wait_for(started.wait(), timeout=2)
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    await asyncio.wait_for(channel.send(source), timeout=2)
    await asyncio.wait_for(channel.send(incoming("next", "again")), timeout=2)
    assert channel.replies == ["next"]


@pytest.mark.asyncio
@pytest.mark.parametrize("backend", ["rust"])
async def test_ambiguous_send_is_not_retried_or_recorded(backend):
    sends = 0

    class TimeoutChannel(FakeChannel):
        async def reply(self, source, content):
            nonlocal sends
            sends += 1
            raise TimeoutError("receipt unknown")

    channel = TimeoutChannel()
    app = AgentChat(channels=[channel], backend=backend)

    @app.on_message
    async def handler(context):
        return "answer"

    source = incoming("unknown", "hello")
    with pytest.raises(TimeoutError):
        await channel.send(source)
    await channel.send(source)
    assert sends == 1
    assert await app.state.history(source.conversation_id) == (source,)


def test_native_boundary_rejects_invalid_transition_without_panicking():
    assert core_version == "0.1.0-alpha.2"
    turn = NativeTurn()
    with pytest.raises(RuntimeError):
        turn.reply_sent()
    turn.claimed(False)


@pytest.mark.asyncio
@pytest.mark.parametrize("backend", ["rust"])
async def test_automatic_reply_preserves_application_override(backend):
    calls = []

    class CustomApp(AgentChat):
        async def reply(self, channel, source, content):
            calls.append(source)
            return await super().reply(channel, source, content.upper())

    channel = FakeChannel()
    app = CustomApp(channels=[channel], backend=backend)

    @app.on_message
    async def handler(context):
        return "answer"

    source = incoming("override", "hello")
    await channel.send(source)
    assert calls == [source]
    assert channel.replies == ["ANSWER"]
    assert len(await app.state.history(source.conversation_id)) == 2


def test_backend_selection_is_explicit_and_immutable(monkeypatch):
    monkeypatch.setenv("AGENTCHAT_BACKEND", "python")
    assert AgentChat(channels=[FakeChannel()]).backend == "rust"
    with pytest.raises(ValueError, match="Python implementation has been removed"):
        AgentChat(channels=[FakeChannel()], backend="python")
    app = AgentChat(channels=[FakeChannel()], backend="rust")
    assert app.backend == "rust"
    with pytest.raises(AttributeError):
        app.backend = "python"
    with pytest.raises(ValueError, match="backend"):
        AgentChat(channels=[FakeChannel()], backend="invalid")


@pytest.mark.asyncio
@pytest.mark.parametrize("backend", ["rust"])
async def test_memory_state_keeps_live_views_and_python_slice_behavior(backend):
    state = MemoryState(backend=backend)
    view = state.messages
    first = incoming("first", "hello")
    second = incoming("second", "again")
    await state.append(first)
    snapshot = view[first.conversation_id]
    await state.append(second)
    assert snapshot == (first,)
    assert view[first.conversation_id] == (first, second)
    assert (await state.history(first.conversation_id))[0] is first
    assert await state.history(first.conversation_id, limit=0) == (first, second)
    assert await state.history(first.conversation_id, limit=-1) == (second,)
    assert await state.claim(first.id) is True
    assert await state.claim(first.id) is False


@pytest.mark.asyncio
async def test_native_memory_releases_cycles_in_opaque_metadata():
    class Payload:
        pass

    state = MemoryState(backend="rust")
    payload = Payload()
    payload.state = state
    source = replace(incoming("cycle", "hello"), metadata={"payload": payload})
    await state.append(source)
    reference = weakref.ref(payload)
    del state, payload, source
    gc.collect()
    assert reference() is None
