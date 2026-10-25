"""Messages that arrive while their conversation's handler is still running."""
import asyncio
from dataclasses import replace

import pytest

from agentchat import AgentChat, MessageContext, Sender
from agentchat.state import MemoryState
from tests.support.channels import FakeChannel, incoming


def app_with(busy=None):
    channel = FakeChannel()
    options = {} if busy is None else {"busy": busy}
    app = AgentChat(channels=[channel], state=MemoryState(), **options)
    return app, channel


async def texts(app):
    return [message.text for message in await app.state.history("fake:conversation")]


@pytest.mark.asyncio
async def test_steer_is_the_default_and_hands_messages_to_the_running_handler():
    app, channel = app_with()
    started, release = asyncio.Event(), asyncio.Event()
    seen = []

    @app.on_message
    async def respond(context: MessageContext) -> str:
        started.set()
        await release.wait()
        seen.append([message.text for message in context.steered()])
        assert context.steered() == ()
        return f"done: {context.message.text}"

    first = asyncio.create_task(channel.send(incoming("1", "draft the report")))
    await started.wait()
    await channel.send(incoming("2", "use last quarter"))
    await channel.send(incoming("3", "and keep it short"))
    release.set()
    await first
    assert app.busy == "steer"
    assert seen == [["use last quarter", "and keep it short"]]
    assert channel.replies == ["done: draft the report"]
    assert await texts(app) == [
        "draft the report", "use last quarter", "and keep it short", "done: draft the report",
    ]


@pytest.mark.asyncio
async def test_unread_steered_messages_run_afterwards_in_order():
    app, channel = app_with("steer")
    started, release = asyncio.Event(), asyncio.Event()
    handled = []

    @app.on_message
    async def respond(context: MessageContext) -> str:
        handled.append(context.message.text)
        if context.message.text == "first":
            started.set()
            await release.wait()
        return f"re: {context.message.text}"

    first = asyncio.create_task(channel.send(incoming("1", "first")))
    await started.wait()
    await channel.send(incoming("2", "second"))
    await channel.send(incoming("3", "third"))
    release.set()
    await first
    # "third" was steered to the turn that handled "second" and is unread there too.
    assert handled == ["first", "second", "third"]
    assert channel.replies == ["re: first", "re: second", "re: third"]


@pytest.mark.asyncio
async def test_a_failed_turn_still_runs_its_unread_messages_then_raises():
    app, channel = app_with()
    started, release = asyncio.Event(), asyncio.Event()

    @app.on_message
    async def respond(context: MessageContext) -> str:
        if context.message.text == "first":
            started.set()
            await release.wait()
            raise RuntimeError("model failed")
        return f"re: {context.message.text}"

    first = asyncio.create_task(channel.send(incoming("1", "first")))
    await started.wait()
    await channel.send(incoming("2", "second"))
    release.set()
    with pytest.raises(RuntimeError, match="model failed"):
        await first
    assert channel.replies == ["re: second"]


@pytest.mark.asyncio
async def test_queue_waits_for_the_running_handler():
    app, channel = app_with("queue")
    started, release = asyncio.Event(), asyncio.Event()
    steered = []

    @app.on_message
    async def respond(context: MessageContext) -> str:
        if context.message.text == "first":
            started.set()
            await release.wait()
        steered.append(context.steered())
        return f"re: {context.message.text}"

    first = asyncio.create_task(channel.send(incoming("1", "first")))
    await started.wait()
    second = asyncio.create_task(channel.send(incoming("2", "second")))
    await asyncio.sleep(0)
    assert channel.replies == []
    release.set()
    await asyncio.gather(first, second)
    assert steered == [(), ()]
    assert channel.replies == ["re: first", "re: second"]
    assert await texts(app) == ["first", "re: first", "second", "re: second"]


@pytest.mark.asyncio
async def test_reject_drops_messages_while_busy_and_not_afterwards():
    app, channel = app_with("reject")
    started, release = asyncio.Event(), asyncio.Event()

    @app.on_message
    async def respond(context: MessageContext) -> str:
        if context.message.text == "first":
            started.set()
            await release.wait()
        return f"re: {context.message.text}"

    first = asyncio.create_task(channel.send(incoming("1", "first")))
    await started.wait()
    await channel.send(incoming("2", "ignored"))
    release.set()
    await first
    await channel.send(incoming("3", "later"))
    assert channel.replies == ["re: first", "re: later"]
    assert "ignored" not in await texts(app)


@pytest.mark.asyncio
async def test_other_conversations_are_never_steered():
    app, channel = app_with()
    started, release = asyncio.Event(), asyncio.Event()

    @app.on_message
    async def respond(context: MessageContext) -> str:
        if context.message.text == "first":
            started.set()
            await release.wait()
        return f"re: {context.message.text}"

    first = asyncio.create_task(channel.send(incoming("1", "first")))
    await started.wait()
    other = replace(incoming("2", "elsewhere"), conversation_id="fake:other")
    await channel.send(other)
    assert channel.replies == ["re: elsewhere"]
    release.set()
    await first


@pytest.mark.asyncio
async def test_stop_reaches_only_the_running_handler():
    app, channel = app_with()
    started = asyncio.Event()
    states = []

    @app.on_message
    async def respond(context: MessageContext) -> str:
        states.append(context.stopped)
        started.set()
        await asyncio.wait_for(context.wait_stopped(), 1)
        states.append(context.stopped)
        return "Stopped."

    assert app.stop("fake:conversation") is False
    first = asyncio.create_task(channel.send(incoming("1", "long task")))
    await started.wait()
    assert app.stop("fake:other") is False
    assert app.stop("fake:conversation") is True
    await first
    assert states == [False, True]
    assert channel.replies == ["Stopped."]
    assert app.stop("fake:conversation") is False


@pytest.mark.asyncio
async def test_a_message_steered_after_stop_starts_a_fresh_turn():
    app, channel = app_with()
    started = asyncio.Event()
    stopped_flags = []

    @app.on_message
    async def respond(context: MessageContext) -> str:
        stopped_flags.append(context.stopped)
        if context.message.text == "first":
            started.set()
            await context.wait_stopped()
            return "Stopped."
        return f"re: {context.message.text}"

    first = asyncio.create_task(channel.send(incoming("1", "first")))
    await started.wait()
    app.stop("fake:conversation")
    await channel.send(incoming("2", "instead do this"))
    await first
    assert stopped_flags == [False, False]
    assert channel.replies == ["Stopped.", "re: instead do this"]


def test_manual_context_has_no_turn_controls():
    app, channel = app_with()
    context = MessageContext(app, channel, incoming("1", "hello"))
    assert context.stopped is False
    assert context.steered() == ()
    with pytest.raises(RuntimeError, match="stopped"):
        context.wait_stopped()


def test_busy_must_be_a_known_policy():
    with pytest.raises(ValueError, match="busy"):
        AgentChat(channels=[FakeChannel()], busy="later")
    assert AgentChat(channels=[FakeChannel()], busy="queue").busy == "queue"


@pytest.mark.asyncio
async def test_steering_keeps_the_original_sender_on_each_message():
    app, channel = app_with()
    started, release = asyncio.Event(), asyncio.Event()
    senders = []

    @app.on_message
    async def respond(context: MessageContext) -> str:
        started.set()
        await release.wait()
        senders.extend(message.sender.id for message in context.steered())
        return "ok"

    first = asyncio.create_task(channel.send(incoming("1", "first")))
    await started.wait()
    await channel.send(replace(incoming("2", "from bob"), sender=Sender("bob")))
    release.set()
    await first
    assert senders == ["bob"]
