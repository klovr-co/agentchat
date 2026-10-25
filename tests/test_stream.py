"""Answers shown while they are written."""
import asyncio

import pytest

from agentchat import AgentChat, Message, MessageContext, Sender
from agentchat.channels.base import StreamingChannel
from agentchat.state import MemoryState
from tests.support.channels import FakeChannel, incoming


class StreamChannel(FakeChannel):
    stream_interval = 0
    stream_limit = None

    def __init__(self) -> None:
        super().__init__()
        self.calls: list[tuple[str, str]] = []
        self.fail_on: str | None = None

    def _call(self, kind: str, text: str) -> None:
        self.calls.append((kind, text))
        if self.fail_on == kind:
            raise RuntimeError(f"{kind} failed")

    async def start_stream(self, source: Message, text: str) -> object:
        self._call("start", text)
        return {"sequence": 1}

    async def append_stream(self, source: Message, stream: object, text: str) -> object:
        self._call("append", text)
        return {"sequence": stream["sequence"] + 1}

    async def stop_stream(self, source: Message, stream: object, text: str) -> Message:
        self._call("stop", text)
        return Message(
            id=f"streamed:{len(self.calls)}",
            conversation_id=source.conversation_id,
            channel=self.name,
            sender=Sender(id="agent"),
            text=text,
            role="assistant",
        )


def streaming_app(channel):
    return AgentChat(channels=[channel], state=MemoryState())


async def history(app):
    return [(m.role, m.text) for m in await app.state.history("fake:conversation")]


@pytest.mark.asyncio
async def test_stream_sends_the_whole_text_so_far_and_confirms_it():
    channel = StreamChannel()
    assert isinstance(channel, StreamingChannel)
    app = streaming_app(channel)

    @app.on_message
    async def respond(context: MessageContext) -> None:
        async with context.stream() as answer:
            for chunk in ["Hello", ", ", "world"]:
                await answer.append(chunk)
        assert answer.text == "Hello, world"
        assert [m.text for m in answer.messages] == ["Hello, world"]

    await channel.send(incoming("1", "hi"))
    assert channel.calls == [
        ("start", "Hello"), ("append", "Hello, "), ("append", "Hello, world"),
        ("stop", "Hello, world"),
    ]
    assert channel.replies == []
    assert await history(app) == [("user", "hi"), ("assistant", "Hello, world")]


@pytest.mark.asyncio
async def test_updates_wait_for_the_interval_and_finish_sends_the_rest():
    channel = StreamChannel()
    app = streaming_app(channel)
    source = incoming("1", "hi")
    async with app.stream(channel, source, interval=60) as answer:
        await answer.append("a")
        await answer.append("b")
        await answer.append("c")
    assert channel.calls == [("start", "a"), ("stop", "abc")]


@pytest.mark.asyncio
async def test_long_answers_continue_in_new_messages_at_a_break():
    channel = StreamChannel()
    channel.stream_limit = 12
    app = streaming_app(channel)
    source = incoming("1", "hi")
    async with app.stream(channel, source) as answer:
        await answer.append("first part ")
        await answer.append("second part ")
        await answer.append("end")
    assert [m.text for m in answer.messages] == ["first part ", "second part ", "end"]
    assert [kind for kind, _ in channel.calls].count("stop") == 3
    assert all(len(text) <= 12 for _, text in channel.calls)


@pytest.mark.asyncio
async def test_channels_without_streaming_get_one_reply():
    channel = FakeChannel()
    assert not isinstance(channel, StreamingChannel)
    app = streaming_app(channel)
    source = incoming("1", "hi")
    async with app.stream(channel, source) as answer:
        await answer.append("one ")
        await answer.append("reply")
    assert channel.replies == ["one reply"]
    assert [m.text for m in answer.messages] == ["one reply"]
    assert await history(app) == [("assistant", "one reply")]


@pytest.mark.asyncio
async def test_an_empty_stream_sends_nothing():
    for channel in (StreamChannel(), FakeChannel()):
        app = streaming_app(channel)
        async with app.stream(channel, incoming("1", "hi")) as answer:
            pass
        assert answer.messages == ()
        assert channel.replies == []
        assert getattr(channel, "calls", []) == []


@pytest.mark.asyncio
async def test_handler_error_closes_the_stream_and_records_nothing():
    channel = StreamChannel()
    app = streaming_app(channel)
    source = incoming("1", "hi")
    with pytest.raises(ValueError, match="model"):
        async with app.stream(channel, source) as answer:
            await answer.append("partial")
            raise ValueError("model failed")
    assert channel.calls == [("start", "partial"), ("stop", "partial")]
    assert await history(app) == []


@pytest.mark.asyncio
async def test_stream_errors_propagate_without_a_second_send():
    channel = StreamChannel()
    channel.fail_on = "append"
    app = streaming_app(channel)
    with pytest.raises(RuntimeError, match="append failed"):
        async with app.stream(channel, incoming("1", "hi")) as answer:
            await answer.append("a")
            await answer.append("b")
    assert [kind for kind, _ in channel.calls] == ["start", "append", "stop"]
    assert channel.replies == []


@pytest.mark.asyncio
async def test_a_finished_stream_refuses_more_text():
    channel = StreamChannel()
    app = streaming_app(channel)
    answer = app.stream(channel, incoming("1", "hi"))
    await answer.append("done")
    message = await answer.finish()
    assert message.text == "done"
    with pytest.raises(RuntimeError, match="finished"):
        await answer.append("more")
    with pytest.raises(RuntimeError, match="finished"):
        await answer.finish()


@pytest.mark.asyncio
async def test_a_stopped_handler_can_end_its_stream_early():
    channel = StreamChannel()
    app = streaming_app(channel)
    started = asyncio.Event()

    @app.on_message
    async def respond(context: MessageContext) -> None:
        async with context.stream() as answer:
            await answer.append("Working")
            started.set()
            await context.wait_stopped()
            await answer.append(" — stopped.")

    turn = asyncio.create_task(channel.send(incoming("1", "hi")))
    await started.wait()
    app.stop("fake:conversation")
    await turn
    assert channel.calls[-1] == ("stop", "Working — stopped.")


def test_interval_must_not_be_negative():
    channel = StreamChannel()
    app = streaming_app(channel)
    with pytest.raises(ValueError, match="interval"):
        app.stream(channel, incoming("1", "hi"), interval=-1)
