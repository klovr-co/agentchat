"""Discord interactions and gateway events beyond chat messages."""

import asyncio
import json
import re
from types import SimpleNamespace

import discord
import pytest

from agentchat import AgentChat
from agentchat.channels.discord import Discord
from agentchat.channels.discord.events import normalize as discord_message
from tests.support.discord import DiscordClient, discord_event
from tests.support.receipts import Receipt


class Client(DiscordClient):
    def __init__(self):
        super().__init__(Receipt(101))
        self.callbacks, self.deleted = [], []

    async def interaction_callback(self, interaction_id, token, payload):
        self.callbacks.append((interaction_id, token, payload))

    async def delete(self, path):
        self.deleted.append(path)


def interaction(kind, data, *, guild_id=None):
    return SimpleNamespace(
        id=555, type=kind, token="tok-1", application_id=77, data=data,
        user=SimpleNamespace(id=30, name="alice", display_name="Alice", bot=False),
        channel_id=10, guild_id=guild_id, channel=SimpleNamespace(),
        message=SimpleNamespace(id=101),
    )


async def until(predicate):
    for _ in range(400):
        if predicate():
            return
        await asyncio.sleep(0.005)
    raise AssertionError("Condition was not reached")


@pytest.mark.asyncio
async def test_interactions_reach_handlers_and_get_their_responses():
    client = Client()
    channel = Discord(bot_token="fake", client=client, send_client=client)
    channel.bind(lambda channel, message: asyncio.sleep(0))
    seen = []

    @channel.on_action(re.compile("^open_"))
    async def open_form(payload):
        seen.append(("action", payload["data"]["custom_id"], payload["token"]))
        return {"type": 9, "data": {"custom_id": "settings", "title": "Settings",
                                    "components": []}}

    @channel.on_action("ack_only")
    async def ack_only(payload):
        seen.append(("ack", payload["user"]["id"]))

    @channel.on_view("settings")
    async def saved(payload):
        seen.append(("view", payload["data"]["custom_id"]))
        return {"type": 4, "data": {"content": "Saved", "flags": 64}}

    @channel.on_command("/deploy")
    async def deploy(payload):
        seen.append(("command", payload["data"]["name"]))

    component = discord.InteractionType.component
    await channel._on_interaction(interaction(component, {"custom_id": "open_settings"}))
    await channel._on_interaction(interaction(component, {"custom_id": "ack_only"}))
    await channel._on_interaction(interaction(discord.InteractionType.modal_submit,
                                              {"custom_id": "settings"}))
    await channel._on_interaction(interaction(discord.InteractionType.application_command,
                                              {"name": "deploy"}))
    await until(lambda: len(client.callbacks) == 4)
    assert sorted(seen) == [("ack", "30"), ("action", "open_settings", "tok-1"),
                            ("command", "deploy"), ("view", "settings")]
    responses = {payload["type"] for _, _, payload in client.callbacks}
    assert responses == {9, 6, 4, 5}
    assert all(call[:2] == ("555", "tok-1") for call in client.callbacks)
    await channel.close()


@pytest.mark.asyncio
async def test_gateway_events_reach_event_handlers():
    client = Client()
    channel = Discord(bot_token="fake", client=client, send_client=client)
    channel.bind(lambda channel, message: asyncio.sleep(0))
    seen = []

    @channel.on_event("GUILD_CREATE")
    async def joined(payload):
        seen.append(payload["d"]["id"])

    await channel._on_gateway(json.dumps({"op": 0, "t": "GUILD_CREATE", "s": 1, "d": {"id": "20"}}))
    await channel._on_gateway(json.dumps({"op": 0, "t": "TYPING_START", "d": {}}))
    await channel._on_gateway(json.dumps({"op": 11}))
    await until(lambda: seen)
    assert seen == ["20"]
    await channel.close()


@pytest.mark.asyncio
async def test_notices_delete_and_task_progress():
    client = Client()
    channel = Discord(bot_token="fake", client=client, send_client=client)
    app = AgentChat(channels=[channel])
    direct = discord_message(discord_event(), bot_id=99)
    guild = discord_message(discord_event(direct=False, mentions=[SimpleNamespace(id=99)]),
                            bot_id=99)
    notice = await app.send_ephemeral(channel, direct, "30", "Only you")
    assert notice.text == "Only you"
    with pytest.raises(ValueError, match="interaction"):
        await app.send_ephemeral(channel, guild, "30", "Only you")
    await app.delete(channel, notice)
    assert client.deleted == ["channels/10/messages/101"]

    async with app.stream(channel, direct, interval=0) as reply:
        await reply.plan("Fixing the build")
        await reply.task("logs", "Read the logs")
        await reply.append("Found it.")
    sent = [payload["content"] for (_, payload), _ in client.receipt.calls]
    assert sent[1:] == ["**Fixing the build**", "Found it."]
    assert client.edits[0][2] == "**Fixing the build**\n⏳ Read the logs"
    assert client.deleted[-1] == "channels/10/messages/101"
    await channel.close()


class DirectClient(Client):
    """Opens direct message channels and keeps pins."""

    def __init__(self, pins=()):
        super().__init__()
        self.pins_listed, self.pinned = [], []
        self.pins_listing = {"items": list(pins), "has_more": False}

    async def open_dm(self, user_id):
        return 500 + int(user_id)

    async def pins(self, channel_id):
        self.pins_listed.append(channel_id)
        return self.pins_listing

    async def pin(self, channel_id, message_id):
        self.pinned.append((channel_id, message_id))


@pytest.mark.asyncio
async def test_notices_fall_back_to_a_direct_message_and_home_is_a_pinned_dm():
    client = DirectClient()
    channel = Discord(bot_token="fake", client=client, send_client=client)
    app = AgentChat(channels=[channel])
    guild = discord_message(
        discord_event(direct=False, mentions=[SimpleNamespace(id=99)]), bot_id=99)
    with pytest.raises(ValueError):
        await app.send_ephemeral(channel, guild, "30", "Only you")
    notice = await app.send_ephemeral(channel, guild, "30", "Only you", fallback="dm")
    channel_id, payload = client.receipt.calls[-1][0]
    assert channel_id == 530 and "message_reference" not in payload
    assert notice.metadata["channel_id"] == 530
    edited = await app.edit(channel, notice, "Changed")
    assert edited.text == "Changed" and client.edits[-1] == (530, 101, "Changed")
    with pytest.raises(ValueError):
        await app.edit(channel, guild, "Not ours")

    home = await channel.publish_home("30", "Welcome")
    assert client.pins_listed == [530] and client.pinned == [(530, 101)]
    again = await channel.publish_home("30", "Welcome back")
    assert again.id == home.id and client.edits[-1] == (530, 101, "Welcome back")

    restarted = DirectClient(pins=[{"message": {"id": "88", "author": {"id": "99"}}}])
    channel = Discord(bot_token="fake", client=restarted, send_client=restarted)
    await channel.publish_home("30", "Still here")
    assert restarted.edits == [(530, 88, "Still here")] and restarted.receipt.calls == []
