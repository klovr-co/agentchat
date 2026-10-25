"""Slack payloads beyond chat messages: handlers, acknowledgements, views and notices."""

import asyncio
import re
import time

import pytest
from slack_sdk.errors import SlackApiError
from slack_sdk.socket_mode.request import SocketModeRequest

from agentchat import AgentChat, Message, Sender
from agentchat.channels.slack import Slack
from agentchat.channels.slack.events import ACK_DEADLINE, responds
from agentchat.channels.slack.socket_mode import SocketMode
from tests.support.slack import FakeSlackClient, channel_event


class Client(FakeSlackClient):
    def __init__(self):
        super().__init__()
        self.calls: list[tuple[str, dict]] = []
        self.statuses: list[dict] = []
        self.refuse: dict[str, str] = {}

    async def _record(self, method, kwargs, response):
        self.calls.append((method, kwargs))
        if method in self.refuse:
            raise SlackApiError("refused", {"ok": False, "error": self.refuse[method]})
        return response

    async def chat_postEphemeral(self, **kwargs):
        return await self._record("chat.postEphemeral", kwargs, {"ok": True, "message_ts": "9.5"})

    async def chat_delete(self, **kwargs):
        return await self._record("chat.delete", kwargs, {"ok": True})

    async def views_open(self, **kwargs):
        return await self._record("views.open", kwargs, {"ok": True, "view": {"id": "V1"}})

    async def views_push(self, **kwargs):
        return await self._record("views.push", kwargs, {"ok": True, "view": {"id": "V2"}})

    async def views_update(self, **kwargs):
        return await self._record("views.update", kwargs, {"ok": True, "view": {"id": "V1"}})

    async def views_publish(self, **kwargs):
        return await self._record("views.publish", kwargs, {"ok": True, "view": {"id": "H1"}})

    async def chat_postMessage(self, **kwargs):
        self.posts.append(kwargs)
        return {"ok": True, "ts": f"9.00000{len(self.posts)}"}

    async def assistant_threads_setStatus(self, **kwargs):
        self.statuses.append(kwargs)
        return {"ok": True}

    async def api_call(self, api_method, *, json=None, **kwargs):
        self.api_calls.append((api_method, json))
        if api_method in self.refuse:
            raise SlackApiError("refused", {"ok": False, "error": self.refuse[api_method]})
        return {"ok": True, "ts": "5.0"} if api_method == "chat.startStream" else {"ok": True}


def slack_with(client=None, **options):
    return Slack(bot_token="test", app_token="test", web_client=client or Client(),
                 workspace_id="team-1", bot_user_id="BOT", **options)


def button(action_id, value="plain"):
    return {"type": "block_actions", "team": {"id": "team-1"}, "trigger_id": "1.2.3",
            "user": {"id": "user-1", "team_id": "team-1"}, "channel": {"id": "C1"},
            "message": {"ts": "9.0"},
            "actions": [{"action_id": action_id, "value": value, "action_ts": "5.1"}]}


def submission(callback_id):
    return {"type": "view_submission", "team": {"id": "team-1"},
            "user": {"id": "user-1", "team_id": "team-1"},
            "view": {"id": "V1", "callback_id": callback_id}}


def mention(ts, text):
    return channel_event(ts, text=text, channel="C1", user="U1")


def thread_source(sender="user-1"):
    return Message(id="slack:team-1:C1:1.0", conversation_id="slack:team-1:C1:1.0",
                   channel="slack", sender=Sender(sender), text="Start", role="user",
                   thread_id="1.0", metadata={"team_id": "team-1", "channel_id": "C1",
                                              "reply_thread_timestamp": "1.0",
                                              "message_timestamp": "1.0"})


@pytest.mark.asyncio
async def test_handlers_receive_the_payloads_agentchat_does_not_handle():
    slack = slack_with()
    seen = []
    messages = []

    async def receive(channel, message):
        messages.append(message)

    slack.bind(receive)

    @slack.on_event("app_home_opened")
    async def home(payload):
        seen.append(("home", payload["event"]["tab"]))

    @slack.on_event("message")
    async def other_message(payload):
        seen.append(("message", payload["event"]["bot_id"]))

    @slack.on_action("retry")
    async def retry(payload):
        seen.append(("retry", payload["trigger_id"]))

    @slack.on_action(re.compile(r"^open_"))
    async def open_any(payload):
        seen.append(("open", payload["actions"][0]["action_id"]))

    @slack.on_command("/deploy")
    async def deploy(payload):
        return {"text": f"Deploying {payload['text']}"}

    @slack.on_raw
    async def raw(payload):
        seen.append(("raw", payload["type"]))

    assert await slack.handle_event({"team_id": "team-1", "event": {
        "type": "app_home_opened", "user": "user-1", "tab": "home"}}) is None
    await slack.handle_event({"team_id": "team-1", "event": {
        "type": "message", "channel": "C1", "bot_id": "B2", "ts": "2.0", "text": "bot",
        "channel_type": "channel"}})
    await slack.handle_event(button("retry"))
    await slack.handle_event(button("open_artifact_7"))
    await slack.handle_event({"type": "shortcut", "team": {"id": "team-1"}, "callback_id": "x"})
    assert await slack.handle_event(
        {"team_id": "team-1", "command": "/deploy", "text": "api"}
    ) == {"text": "Deploying api"}
    # Another workspace's payloads are dropped.
    foreign = button("retry")
    foreign["team"]["id"] = foreign["user"]["team_id"] = "team-2"
    await slack.handle_event(foreign)
    # Prompt presses and chat messages still go to AgentChat.
    await slack.handle_event(button("agentchat_yes", "ac:0123456789abcdef:yes"))
    await slack.handle_event(channel_event("3.0", text="<@BOT> hi"))
    assert seen == [
        ("home", "home"), ("message", "B2"), ("retry", "1.2.3"),
        ("open", "open_artifact_7"), ("raw", "shortcut"),
    ]
    assert [m.metadata.get("agentchat_answer") for m in messages] == [
        {"prompt": "0123456789abcdef", "choice": "yes"}, None,
    ]

    with pytest.raises(TypeError):
        slack.on_action(42)
    with pytest.raises(TypeError):
        slack.on_event(re.compile("x"))
    with pytest.raises(TypeError):
        slack.on_raw("not callable")


@pytest.mark.asyncio
async def test_view_submissions_answer_within_the_deadline():
    slack = slack_with()
    slack.bind(lambda channel, message: asyncio.sleep(0))
    finished = asyncio.Event()

    @slack.on_view("settings")
    async def settings(payload):
        return {"response_action": "errors", "errors": {"name": "Required"}}

    @slack.on_view(re.compile("^slow"))
    async def slow(payload):
        await asyncio.sleep(ACK_DEADLINE + 0.3)
        finished.set()
        return {"response_action": "clear"}

    assert responds(submission("settings")) and not responds(button("retry"))
    assert await slack.handle_event(submission("settings")) == {
        "response_action": "errors", "errors": {"name": "Required"}}
    assert await slack.handle_event(submission("nobody")) is None
    started = time.monotonic()
    assert await slack.handle_event(submission("slow_form")) is None
    assert ACK_DEADLINE - 0.1 <= time.monotonic() - started < ACK_DEADLINE + 0.3
    # The late handler keeps running; closing waits for it.
    await slack.close()
    assert finished.is_set()


class FakeSocket:
    def __init__(self):
        self.socket_mode_request_listeners = []
        self.started = asyncio.Event()
        self.responses = []

    async def connect(self):
        self.started.set()

    async def is_connected(self):
        return True

    async def close(self):
        pass

    async def send_socket_mode_response(self, response):
        self.responses.append((response.envelope_id, response.payload))

    async def emit(self, envelope_id, payload, request_type="interactive"):
        request = SocketModeRequest(type=request_type, envelope_id=envelope_id, payload=payload)
        await self.socket_mode_request_listeners[0](self, request)


@pytest.mark.asyncio
async def test_socket_mode_acknowledges_with_the_handlers_answer(monkeypatch):
    socket = FakeSocket()
    monkeypatch.setattr(
        "agentchat.channels.slack.socket_mode.SocketModeClient", lambda **kwargs: socket,
    )
    received = []

    async def receive(payload):
        received.append(payload.get("type") or payload.get("command"))
        if payload.get("type") == "view_submission":
            return {"response_action": "clear"}
        if payload.get("command"):
            return {"text": "ok"}
        return None

    source = SocketMode(app_token="xapp-test")
    run = asyncio.create_task(source.run(receive))
    await asyncio.wait_for(socket.started.wait(), timeout=1)
    try:
        await socket.emit("submit", submission("settings"))
        await socket.emit("command", {"team_id": "team-1", "command": "/deploy"},
                          request_type="slash_commands")
        await socket.emit("press", button("retry"))
        await asyncio.sleep(0.05)
        assert socket.responses == [
            ("submit", {"response_action": "clear"}),
            ("command", {"text": "ok"}),
            ("press", None),
        ]
        assert received == ["view_submission", "/deploy", "block_actions"]
    finally:
        await source.close()
        await asyncio.wait_for(run, timeout=1)


@pytest.mark.asyncio
async def test_slack_native_stop_stops_only_the_starters_turn():
    client = Client()
    slack = slack_with(client)
    app = AgentChat(channels=[slack])
    started, events = asyncio.Event(), []

    @app.on_message
    async def respond(context):
        started.set()
        await context.wait_stopped()
        return "Stopped."

    @slack.on_event("agent_session_stopped")
    async def stopped(payload):
        events.append(payload["event"]["user"])

    def stop(user, ts):
        return {"team_id": "team-1", "event": {
            "type": "agent_session_stopped", "channel": "C1", "thread_ts": "1.0",
            "user": user, "event_ts": ts, "streaming_message_ts": []}}

    turn = asyncio.create_task(slack.handle_event(mention("1.0", "<@BOT> work")))
    await asyncio.wait_for(started.wait(), timeout=1)
    await slack.handle_event(stop("someone-else", "2.0"))
    assert not turn.done()
    await slack.handle_event(stop("U1", "3.0"))
    await asyncio.wait_for(turn, timeout=1)
    assert events == ["someone-else", "U1"]
    assert client.posts[-1]["text"] == "Stopped."
    assert client.statuses[-1] == {"channel_id": "C1", "thread_ts": "1.0", "status": ""}
    history = await app.state.history("slack:team-1:C1:1.0")
    assert [m.text for m in history] == ["work", "Stopped."]


@pytest.mark.asyncio
async def test_agent_sessions_set_a_lifecycle_status():
    client = Client()
    slack = slack_with(client, agent_sessions=True)
    assert slack.agent_sessions
    assert await slack.set_status(thread_source(), "is reading…")
    assert await slack.set_status(thread_source(), "")
    assert client.api_calls == [
        ("agents.sessions.setStatus", {"channel_id": "C1", "thread_ts": "1.0",
                                       "status": "processing"}),
        ("agents.sessions.setStatus", {"channel_id": "C1", "thread_ts": "1.0",
                                       "status": "active"}),
    ]
    assert client.statuses == []


@pytest.mark.asyncio
async def test_ephemeral_notices_are_private_and_fail_closed():
    client = Client()
    slack = slack_with(client)
    app = AgentChat(channels=[slack])
    source = thread_source()
    sent = await app.send_ephemeral(slack, source, "U1", "Only you", blocks=[
        {"type": "section", "text": {"type": "mrkdwn", "text": "Only you"}}])
    assert sent.metadata["ephemeral"] is True
    assert sent.metadata["message_timestamp"] == "9.5"
    method, request = client.calls[-1]
    assert (method, request["channel"], request["user"], request["thread_ts"]) == (
        "chat.postEphemeral", "C1", "U1", "1.0")
    assert await app.state.history(source.conversation_id) == ()
    with pytest.raises(ValueError, match="ephemeral"):
        await app.delete(slack, sent)
    client.refuse["chat.postEphemeral"] = "user_not_in_channel"
    with pytest.raises(SlackApiError):
        await slack.send_ephemeral(source, "U1", "Only you")
    assert client.posts == []

    class Public:
        name = "public"

        def bind(self, receiver):
            pass

        async def reply(self, source, content):
            raise AssertionError("never posted publicly")

    with pytest.raises(TypeError, match="ephemeral"):
        await AgentChat(channels=[Public()]).send_ephemeral(Public(), source, "U1", "x")
    with pytest.raises(TypeError, match="deleting"):
        await AgentChat(channels=[Public()]).delete(Public(), source)


@pytest.mark.asyncio
async def test_context_replies_privately_to_its_sender():
    client = Client()
    slack = slack_with(client)
    app = AgentChat(channels=[slack])

    @app.on_message
    async def respond(context):
        await context.reply_ephemeral("Only you")
        await context.reply_ephemeral("Someone else", user_id="U2")

    await slack.handle_event(mention("1.0", "<@BOT> hi"))
    users = [request["user"] for method, request in client.calls if method == "chat.postEphemeral"]
    assert users == ["U1", "U2"]


@pytest.mark.asyncio
async def test_views_and_delete_call_slack():
    client = Client()
    slack = slack_with(client)
    modal = {"type": "modal", "title": {"type": "plain_text", "text": "Settings"}}
    assert await slack.open_view("1.2.3", modal) == {"id": "V1"}
    assert await slack.push_view("1.2.4", modal) == {"id": "V2"}
    await slack.update_view(modal, external_id="mine", hash="h")
    await slack.publish_home("U1", {"type": "home", "blocks": []})
    with pytest.raises(ValueError):
        await slack.update_view(modal)
    with pytest.raises(ValueError):
        await slack.publish_home("U1", modal)
    with pytest.raises(ValueError):
        await slack.open_view("", modal)
    sent = await slack.reply(thread_source(), "Working…")
    await slack.delete(sent)
    assert [method for method, _ in client.calls] == [
        "views.open", "views.push", "views.update", "views.publish", "chat.delete"]
    assert client.calls[2][1] == {"view": modal, "external_id": "mine", "hash": "h"}
    assert client.calls[-1][1] == {"channel": "C1", "ts": "9.000001"}
    client.refuse["views.open"] = "expired_trigger_id"
    with pytest.raises(SlackApiError):
        await slack.open_view("1.2.3", modal)


@pytest.mark.asyncio
async def test_streams_show_tasks_as_chunks_and_fall_back_to_text():
    client = Client()
    slack = slack_with(client)
    app = AgentChat(channels=[slack])
    async with app.stream(slack, thread_source(), interval=0) as reply:
        await reply.plan("Fixing the build")
        await reply.task("t1", "Read <@U1> logs", "in_progress")
        await reply.append("Found it.")
        await reply.task("t1", "Read <@U1> logs", "complete", output="1 error")
        with pytest.raises(ValueError):
            await reply.task("t1", "x", "done")
    methods = [(method, request) for method, request in client.api_calls]
    start = methods[0][1]
    assert methods[0][0] == "chat.startStream"
    assert start["task_display_mode"] == "plan"
    assert start["chunks"] == [{"type": "plan_update", "title": "Fixing the build"}]
    assert [request["chunks"] for method, request in methods[1:4]] == [
        [{"type": "task_update", "id": "t1", "title": "Read &lt;@U1&gt; logs",
          "status": "in_progress"}],
        [{"type": "markdown_text", "text": "Found it."}],
        [{"type": "task_update", "id": "t1", "title": "Read &lt;@U1&gt; logs",
          "status": "complete", "output": "1 error"}],
    ]
    assert methods[-1] == ("chat.stopStream", {"channel": "C1", "ts": "5.0"})

    # A workspace that refuses the chunks gets a plain text stream.
    client = Client()
    client.refuse["chat.startStream"] = "invalid_arguments"
    slack = slack_with(client)
    app = AgentChat(channels=[slack])
    reply = app.stream(slack, thread_source(), interval=0)
    await reply.task("t1", "Search")
    client.refuse.clear()
    await reply.append("Answer")
    await reply.task("t1", "Search", "complete")
    await reply.finish()
    assert [method for method, _ in client.api_calls] == [
        "chat.startStream", "chat.startStream", "chat.stopStream"]
    assert client.api_calls[1][1]["markdown_text"] == "Answer"
