"""Teams activities beyond chat messages: handlers, invokes, notices, progress and Stop."""

import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock

import aiohttp
import pytest
from microsoft_agents.activity import Activity, ResourceResponse
from microsoft_agents.hosting.aiohttp import CloudAdapter
from microsoft_agents.hosting.core import OutboundHostValidator

from agentchat import AgentChat
from agentchat.channels.teams import Teams
from agentchat.channels.teams.events import normalize
from tests.support.teams import APP_ID, TENANT_ID, teams_event

CANCELED = ('{"code":"ContentStreamNotAllowed",'
            '"message":"Content stream was canceled by user."}')


class Connector:
    def __init__(self):
        self.sent, self.updated, self.deleted = [], [], []
        self.fail_on = None
        self.close = AsyncMock()
        self.conversations = SimpleNamespace(
            send_to_conversation=self.send, reply_to_activity=self.send,
            update_activity=self.update, delete_activity=self.delete,
        )

    async def send(self, *args):
        activity = args[-1]
        self.sent.append(activity)
        if self.fail_on and self.fail_on(activity):
            raise aiohttp.ClientResponseError(
                SimpleNamespace(real_url="https://x"), (), status=403,
                message=CANCELED)
        return ResourceResponse.model_construct(id=f"a{len(self.sent)}")

    async def update(self, conversation_id, activity_id, activity):
        self.updated.append((activity_id, activity))
        return ResourceResponse.model_construct(id=activity_id)

    async def delete(self, conversation_id, activity_id):
        self.deleted.append(activity_id)


def make_channel():
    connector = Connector()
    factory = SimpleNamespace(
        create_connector_client=AsyncMock(return_value=connector),
        create_user_token_client=AsyncMock(return_value=SimpleNamespace(close=AsyncMock())),
    )
    adapter = CloudAdapter(channel_service_client_factory=factory,
                           host_validator=OutboundHostValidator(enabled=True))
    channel = Teams(app_id=APP_ID, tenant_id=TENANT_ID, app_password="fake", adapter=adapter)
    return channel, connector


def turn(activity):
    context = SimpleNamespace(activity=Activity.model_validate(activity), send_activity=AsyncMock())
    return context


async def until(predicate):
    for _ in range(400):
        if predicate():
            return
        await asyncio.sleep(0.005)
    raise AssertionError("Condition was not reached")


@pytest.mark.asyncio
async def test_invokes_events_and_commands_reach_handlers():
    channel, _ = make_channel()
    seen, messages = [], []

    async def receive(_, message):
        messages.append(message.text)

    channel.bind(receive)

    @channel.on_action("settings")
    async def open_dialog(payload):
        seen.append(("action", payload["name"]))
        return {"task": {"type": "continue", "value": {"title": "Settings"}}}

    @channel.on_view("settings")
    async def submitted(payload):
        seen.append(("view", payload["value"]["data"]["name"]))

    @channel.on_event("conversationUpdate")
    async def joined(payload):
        seen.append(("event", payload["type"]))

    @channel.on_command("/deploy")
    async def deploy(payload):
        seen.append(("command", payload["text"]))

    fetch = turn(teams_event(type="invoke", name="task/fetch", text=None,
                             value={"data": {"action_id": "settings"}}))
    await channel.on_turn(fetch)
    response = fetch.send_activity.await_args.args[0]
    assert response.type == "invokeResponse"
    assert response.value["status"] == 200
    assert response.value["body"]["task"]["type"] == "continue"

    submit = turn(teams_event(type="invoke", name="task/submit", text=None,
                              value={"data": {"callback_id": "settings", "name": "x"}}))
    await channel.on_turn(submit)
    assert submit.send_activity.await_args.args[0].value == {"status": 200}

    await channel.on_turn(turn(teams_event(type="conversationUpdate", text=None)))
    await channel.on_turn(turn(teams_event(text="/deploy api")))
    await channel.on_turn(turn(teams_event(text="hello")))
    foreign = teams_event(type="conversationUpdate", text=None,
                          channelData={"tenant": {"id": "other"}})
    await channel.on_turn(turn(foreign))
    await until(lambda: len(seen) == 4 and messages)
    assert sorted(seen) == [("action", "task/fetch"), ("command", "/deploy api"),
                            ("event", "conversationUpdate"), ("view", "x")]
    assert messages == ["hello"]
    await channel.close()


@pytest.mark.asyncio
async def test_targeted_notices_and_delete():
    channel, connector = make_channel()
    app = AgentChat(channels=[channel])
    group = normalize(teams_event(kind="groupChat", text="<at>Bot</at> hi", entities=[
        {"type": "mention", "text": "<at>Bot</at>", "mentioned": {"id": f"28:{APP_ID}"}}]),
        app_id=APP_ID, tenant_id=TENANT_ID)
    personal = normalize(teams_event(), app_id=APP_ID, tenant_id=TENANT_ID)
    notice = await app.send_ephemeral(channel, group, "29:bob", "Only you")
    sent = connector.sent[-1]
    assert sent.recipient.id == "29:bob"
    assert sent.is_targeted_activity()
    assert notice.metadata["ephemeral"] is True
    reply = await app.send_ephemeral(channel, personal, "29:alice", "Only you")
    assert not connector.sent[-1].is_targeted_activity()
    with pytest.raises(ValueError):
        await app.send_ephemeral(channel, personal, "29:bob", "Not yours")
    await app.delete(channel, reply)
    assert connector.deleted == [reply.metadata["activity_id"]]
    await channel.close()


@pytest.mark.asyncio
async def test_group_progress_is_a_message_and_personal_progress_is_informative():
    channel, connector = make_channel()
    app = AgentChat(channels=[channel])
    group = normalize(teams_event(kind="groupChat", text="<at>Bot</at> hi", entities=[
        {"type": "mention", "text": "<at>Bot</at>", "mentioned": {"id": f"28:{APP_ID}"}}]),
        app_id=APP_ID, tenant_id=TENANT_ID)
    async with app.stream(channel, group, interval=0) as reply:
        await reply.plan("Fixing the build")
        await reply.task("logs", "Read the logs")
        await reply.append("Found it.")
    assert connector.sent[0].text == "**Fixing the build**"
    assert connector.updated[0][1].text == "**Fixing the build**\n⏳ Read the logs"
    assert connector.deleted == ["a1"]

    channel, connector = make_channel()
    app = AgentChat(channels=[channel])
    personal = normalize(teams_event(), app_id=APP_ID, tenant_id=TENANT_ID)
    async with app.stream(channel, personal, interval=0) as reply:
        await reply.task("search", "Searching the docs")
        await reply.append("Answer")
    first = connector.sent[0]
    assert first.text == "Searching the docs"
    assert first.entities[0].stream_type == "informative"
    assert connector.sent[1].entities[0].stream_type == "streaming"
    assert connector.sent[-1].entities[0].stream_type == "final"
    await channel.close()


@pytest.mark.asyncio
async def test_teams_stop_button_stops_the_turn():
    channel, connector = make_channel()
    app = AgentChat(channels=[channel])
    started, stopped = asyncio.Event(), asyncio.Event()

    @app.on_message
    async def respond(context):
        async with context.stream(interval=0) as reply:
            await reply.append("Working")
            started.set()
            await reply.append(" more")
            await asyncio.wait_for(context.wait_stopped(), 2)
            stopped.set()

    connector.fail_on = lambda activity: activity.text == "Working more"
    await channel.on_turn(turn(teams_event()))
    await asyncio.wait_for(stopped.wait(), 3)
    await until(lambda: not channel._delivery.tasks)
    # Nothing is sent after Stop: no final message.
    assert all(a.entities[0].stream_type != "final" for a in connector.sent if a.entities)
    await channel.close()
