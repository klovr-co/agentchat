"""Questions with buttons: approvals, Stop, and Queue or Steer."""
import asyncio
from dataclasses import replace
from types import MappingProxyType

import pytest

from agentchat import (
    AgentChat,
    Answer,
    Choice,
    Message,
    MessageContext,
    Prompt,
    Sender,
    prompt_token,
)
from agentchat.channels.base import InteractiveChannel
from agentchat.state import MemoryState
from tests.support.channels import FakeChannel, incoming


class ButtonChannel(FakeChannel):
    def __init__(self) -> None:
        super().__init__()
        self.prompts: list[Prompt] = []
        self.closed: list[tuple[str, str | None]] = []
        self.shown = asyncio.Event()

    async def send_prompt(self, source: Message, prompt: Prompt) -> Message:
        self.prompts.append(prompt)
        self.shown.set()
        return replace(source, id=f"prompt:{prompt.id}", role="assistant", text=prompt.text)

    async def close_prompt(self, source, sent, prompt, answer) -> None:
        self.closed.append((prompt.id, answer.choice if answer else None))

    async def press(self, choice: str, *, by: str = "user", prompt: Prompt | None = None,
                    conversation: str = "fake:conversation", press_id: str | None = None):
        prompt = prompt or self.prompts[-1]
        token = prompt_token(prompt.id, choice)
        message = Message(
            id=press_id or f"press:{prompt.id}:{choice}:{by}",
            conversation_id=conversation,
            channel=self.name,
            sender=Sender(id=by),
            text=choice,
            role="user",
            metadata=MappingProxyType({"agentchat_answer": {
                "prompt": token.split(":")[1], "choice": token.split(":")[2],
            }}),
        )
        await self.send(message)


def app_with(channel, **options):
    return AgentChat(channels=[channel], state=MemoryState(), **options)


async def wait_for_prompt(channel, count=1):
    for _ in range(1000):
        if len(channel.prompts) >= count:
            return
        await asyncio.sleep(0)
    raise AssertionError("the prompt was never shown")


@pytest.mark.asyncio
async def test_ask_returns_the_senders_choice_and_closes_the_prompt():
    channel = ButtonChannel()
    assert isinstance(channel, InteractiveChannel)
    app = app_with(channel)
    answers = []

    @app.on_message
    async def respond(context: MessageContext) -> str:
        answer = await context.ask(
            "Delete build/?", [Choice("yes", "Approve", "primary"), Choice("no", "Deny")]
        )
        answers.append(answer)
        return "Deleted." if answer.choice == "yes" else "Kept."

    turn = asyncio.create_task(channel.send(incoming("1", "clean up")))
    await wait_for_prompt(channel)
    prompt = channel.prompts[0]
    assert (prompt.text, [c.id for c in prompt.choices]) == ("Delete build/?", ["yes", "no"])
    assert len(prompt.id) == 16
    await channel.press("yes")
    await turn
    assert answers == [Answer(prompt.id, "yes", "Approve", Sender("user"))]
    assert channel.replies == ["Deleted."]
    assert channel.closed == [(prompt.id, "yes")]
    history = [m.text for m in await app.state.history("fake:conversation")]
    assert history == ["clean up", "Deleted."]


@pytest.mark.asyncio
async def test_presses_from_others_other_conversations_or_unknown_choices_are_ignored():
    channel = ButtonChannel()
    app = app_with(channel)
    source = incoming("1", "hi")
    asked = asyncio.create_task(app.ask(channel, source, "Run it?", ["Yes", "No"]))
    await wait_for_prompt(channel)
    await channel.press("1", by="mallory")
    await channel.press("1", conversation="fake:other")
    await channel.press("3")
    await asyncio.sleep(0)
    assert not asked.done()
    await channel.press("2")
    answer = await asked
    assert (answer.choice, answer.label) == ("2", "No")
    await channel.press("1", press_id="late")
    assert channel.closed == [(channel.prompts[0].id, "2")]


@pytest.mark.asyncio
async def test_allowed_widens_who_may_answer():
    channel = ButtonChannel()
    app = app_with(channel)
    source = incoming("1", "hi")
    asked = asyncio.create_task(app.ask(channel, source, "Ship?", ["Yes"], allowed=["lead"]))
    await wait_for_prompt(channel)
    await channel.press("1", by="user")
    assert not asked.done()
    await channel.press("1", by="lead")
    assert (await asked).sender == Sender("lead")
    asked = asyncio.create_task(app.ask(channel, source, "Lunch?", ["Yes"], allowed="anyone"))
    await wait_for_prompt(channel, 2)
    await channel.press("1", by="someone")
    assert (await asked).sender == Sender("someone")


@pytest.mark.asyncio
async def test_unanswered_prompts_time_out_closed_and_never_approve():
    channel = ButtonChannel()
    app = app_with(channel)
    source = incoming("1", "hi")
    with pytest.raises(TimeoutError, match="answered"):
        await app.ask(channel, source, "Run it?", ["Yes"], timeout=0.01)
    prompt = channel.prompts[0]
    assert channel.closed == [(prompt.id, None)]
    await channel.press("1")
    assert channel.closed == [(prompt.id, None)]


@pytest.mark.asyncio
async def test_channels_without_buttons_refuse_to_ask():
    channel = FakeChannel()
    app = app_with(channel)
    with pytest.raises(TypeError, match="prompts"):
        await app.ask(channel, incoming("1", "hi"), "Run it?", ["Yes"])


@pytest.mark.asyncio
@pytest.mark.parametrize(("text", "choices"), [
    ("", ["Yes"]),
    ("Pick", []),
    ("Pick", [str(i) for i in range(11)]),
    ("Pick", [Choice("same", "A"), Choice("same", "B")]),
    ("Pick", [Choice("has space", "A")]),
])
async def test_invalid_prompts_fail_before_anything_is_shown(text, choices):
    channel = ButtonChannel()
    app = app_with(channel)
    with pytest.raises(ValueError):
        await app.ask(channel, incoming("1", "hi"), text, choices)
    assert channel.prompts == []


@pytest.mark.asyncio
async def test_presses_never_start_turns_or_enter_history():
    channel = ButtonChannel()
    app = app_with(channel)
    calls = []

    @app.on_message
    async def respond(context: MessageContext) -> None:
        calls.append(context.message.text)

    source = incoming("1", "hi")
    asked = asyncio.create_task(app.ask(channel, source, "Run it?", ["Yes"]))
    await wait_for_prompt(channel)
    await channel.press("1")
    await asked
    assert calls == []
    assert await app.state.history("fake:conversation") == ()


@pytest.mark.asyncio
async def test_stop_button_stops_the_turn_and_goes_away_when_it_ends():
    channel = ButtonChannel()
    app = app_with(channel)

    @app.on_message
    async def respond(context: MessageContext) -> str:
        shown = await context.offer_stop()
        assert shown is not None
        await context.wait_stopped()
        return "Stopped."

    turn = asyncio.create_task(channel.send(incoming("1", "long job")))
    await wait_for_prompt(channel)
    prompt = channel.prompts[0]
    assert [(c.id, c.label, c.style) for c in prompt.choices] == [("stop", "Stop", "danger")]
    await channel.press("stop", by="mallory")
    await asyncio.sleep(0)
    assert not turn.done()
    await channel.press("stop")
    await turn
    assert channel.replies == ["Stopped."]
    assert channel.closed == [(prompt.id, "stop")]


@pytest.mark.asyncio
async def test_an_unpressed_stop_button_is_closed_when_the_turn_ends():
    channel = ButtonChannel()
    app = app_with(channel)

    @app.on_message
    async def respond(context: MessageContext) -> str:
        await context.offer_stop(label="Cancel")
        return "Done."

    await channel.send(incoming("1", "quick job"))
    assert channel.closed == [(channel.prompts[0].id, None)]
    assert app.stop("fake:conversation") is False


@pytest.mark.asyncio
async def test_stop_button_is_skipped_on_channels_without_buttons():
    channel = FakeChannel()
    app = app_with(channel)

    @app.on_message
    async def respond(context: MessageContext) -> str:
        assert await context.offer_stop() is None
        return "ok"

    await channel.send(incoming("1", "hi"))
    with pytest.raises(RuntimeError, match="running"):
        await app.offer_stop(channel, incoming("2", "idle"))


async def busy_scenario(channel, app, press=None):
    started, release = asyncio.Event(), asyncio.Event()
    seen = []

    @app.on_message
    async def respond(context: MessageContext) -> str:
        if context.message.text == "first":
            started.set()
            await release.wait()
            seen.append(("steered", [m.text for m in context.steered()]))
        else:
            seen.append(("turn", context.message.text))
        return f"re: {context.message.text}"

    first = asyncio.create_task(channel.send(incoming("1", "first")))
    await started.wait()
    await channel.send(replace(incoming("2", "second"), sender=Sender("bob")))
    if press:
        await wait_for_prompt(channel)
        await channel.press(press, by="bob")
    release.set()
    await first
    return seen


@pytest.mark.asyncio
async def test_busy_ask_lets_the_sender_steer():
    channel = ButtonChannel()
    app = app_with(channel, busy="ask")
    seen = await busy_scenario(channel, app, press="steer")
    assert seen == [("steered", ["second"])]
    prompt = channel.prompts[0]
    assert [c.id for c in prompt.choices] == ["steer", "queue"]
    assert channel.closed == [(prompt.id, "steer")]


@pytest.mark.asyncio
async def test_busy_ask_lets_the_sender_queue():
    channel = ButtonChannel()
    app = app_with(channel, busy="ask")
    seen = await busy_scenario(channel, app, press="queue")
    assert seen == [("steered", []), ("turn", "second")]
    assert channel.replies == ["re: first", "re: second"]


@pytest.mark.asyncio
async def test_busy_ask_queues_when_nobody_answers_in_time():
    channel = ButtonChannel()
    app = app_with(channel, busy="ask")
    seen = await busy_scenario(channel, app)
    assert seen == [("steered", []), ("turn", "second")]
    assert channel.closed == [(channel.prompts[0].id, None)]
    texts = [m.text for m in await app.state.history("fake:conversation")]
    assert texts == ["first", "re: first", "second", "re: second"]


@pytest.mark.asyncio
async def test_busy_ask_steers_on_channels_without_buttons():
    channel = FakeChannel()
    app = app_with(channel, busy="ask")
    assert app.busy == "ask"
    seen = await busy_scenario(channel, app)
    assert seen == [("steered", ["second"])]


def test_prompt_tokens_are_short_and_checked():
    assert prompt_token("0123456789abcdef", "approve") == "ac:0123456789abcdef:approve"
    with pytest.raises(ValueError):
        prompt_token("x", "has space")
