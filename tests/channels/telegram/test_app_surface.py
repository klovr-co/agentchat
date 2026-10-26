"""Telegram updates beyond chat messages: handlers, notices, delete, progress and drafts."""

import asyncio
import re

import pytest

from agentchat import AgentChat
from agentchat.channels.telegram.transport import TelegramError
from tests.support.telegram import (
    GROUP,
    TelegramClient,
    callback_update,
    faked,
    group_update,
    telegram_update,
    until,
)


class Client(TelegramClient):
    async def call(self, method, payload=None, **options):
        if method == "sendMessage" and payload and "ephemeral_message_parameters" in payload:
            self.calls.append((method, payload))
            return {"message_id": 0, "ephemeral_message_id": 77}
        return await super().call(method, payload, **options)


async def running(channel, client):
    task = asyncio.create_task(channel.run())
    await until(lambda: client.started.is_set())
    return task


@pytest.mark.asyncio
async def test_handlers_take_presses_commands_and_other_updates():
    member = {"update_id": 9, "my_chat_member": {"chat": dict(GROUP), "new_chat_member": {}}}
    client = Client(batches=[[
        callback_update(data="retry", update_id=5),
        telegram_update(6, text="/start@agentchat_bot now"),
        member,
        telegram_update(7, text="/help"),
        callback_update(data="no", update_id=8),
    ]])
    channel = faked(client)
    seen, messages = [], []

    async def receive(channel, message):
        messages.append(message.text)

    channel.bind(receive)

    @channel.on_action(re.compile("^ret"))
    async def retry(payload):
        seen.append(("retry", payload["callback_query"]["id"]))
        return {"text": "Retrying", "show_alert": True}

    @channel.on_command("/start")
    async def start(payload):
        seen.append(("start", payload["message"]["text"]))

    @channel.on_event("my_chat_member")
    async def joined(payload):
        seen.append(("joined", payload["update_id"]))

    task = await running(channel, client)
    try:
        await until(lambda: len(seen) == 3 and len(messages) == 2
                    and len(client.method_calls("answerCallbackQuery")) == 2)
        assert sorted(seen) == [("joined", 9), ("retry", "cb-1"),
                                ("start", "/start@agentchat_bot now")]
        # Without a handler, a command and a press stay ordinary messages.
        assert sorted(messages) == ["/help", "No"]
        assert {"callback_query_id": "cb-1", "text": "Retrying", "show_alert": True} in \
            client.method_calls("answerCallbackQuery")
        allowed = client.method_calls("getUpdates")[0]["allowed_updates"]
        assert allowed == ["message", "callback_query", "my_chat_member"]
    finally:
        await channel.close()
        await task


@pytest.mark.asyncio
async def test_notices_are_ephemeral_in_groups_and_replies_in_private_chats():
    client = Client()
    channel = faked(client)
    app = AgentChat(channels=[channel])
    await channel._load_bot_identity()
    group = channel._normalize(group_update())
    private = channel._normalize(telegram_update())
    notice = await app.send_ephemeral(channel, group, "30", "Only you")
    assert notice.metadata["ephemeral"] is True
    request = client.method_calls("sendMessage")[-1]
    assert request["ephemeral_message_parameters"] == {"receiver_user_id": 30}
    reply = await app.send_ephemeral(channel, private, "30", "Only you")
    assert "ephemeral_message_parameters" not in client.method_calls("sendMessage")[-1]
    assert reply.metadata["message_id"] == 101
    with pytest.raises(ValueError):
        await app.send_ephemeral(channel, private, "31", "Not yours")
    assert await app.state.history(group.conversation_id) == ()

    await app.delete(channel, notice)
    await app.delete(channel, reply)
    assert client.method_calls("deleteEphemeralMessage") == [
        {"chat_id": -1001, "receiver_user_id": 30, "ephemeral_message_id": 77}]
    assert client.method_calls("deleteMessage") == [{"chat_id": 30, "message_id": 101}]


@pytest.mark.asyncio
async def test_task_progress_is_a_separate_message_removed_at_the_end():
    client = Client()
    channel = faked(client)
    app = AgentChat(channels=[channel])
    await channel._load_bot_identity()
    source = channel._normalize(telegram_update())
    async with app.stream(channel, source, interval=0) as reply:
        await reply.plan("Fixing the build")
        await reply.task("logs", "Read the logs")
        await reply.append("Found it.")
        await reply.task("logs", "Read the logs", "complete", output="1 error")
    sends = client.method_calls("sendMessage")
    # The first update shows the plan; later ones edit that message.
    assert sends[0]["text"] == "Fixing the build"
    assert sends[1]["text"] == "Found it."
    edits = [call["text"] for call in client.method_calls("editMessageText")]
    assert edits == ["Fixing the build\n⏳ Read the logs",
                     "Fixing the build\n✅ Read the logs — 1 error"]
    assert client.method_calls("deleteMessage") == [{"chat_id": 30, "message_id": 101}]
    history = await app.state.history(source.conversation_id)
    assert [message.text for message in history] == ["Found it."]


@pytest.mark.asyncio
async def test_drafts_stream_in_private_chats_and_their_stop_button_stops_the_turn():
    client = Client()
    channel = faked(client, stream_drafts=True)
    app = AgentChat(channels=[channel])
    started, stopped = asyncio.Event(), asyncio.Event()

    @app.on_message
    async def respond(context):
        async with context.stream(interval=0) as reply:
            await reply.append("Working")
            started.set()
            await context.wait_stopped()
            stopped.set()
            await reply.append(" (stopped)")

    client.batches = [[telegram_update(1, text="go")]]
    task = await running(channel, client)
    try:
        await asyncio.wait_for(started.wait(), 2)
        drafts = client.method_calls("sendMessageDraft")
        assert drafts[0]["can_stop"] is True and drafts[0]["text"] == "Working"
        channel._has_admitted_all([{"update_id": 2, "stopped_message_generation": {
            "chat": {"id": 30, "type": "private"}, "draft_id": drafts[0]["draft_id"]}}])
        await asyncio.wait_for(stopped.wait(), 2)
        await until(lambda: client.method_calls("sendMessage"))
        assert client.method_calls("sendMessage")[-1]["text"] == "Working (stopped)"
        allowed = client.method_calls("getUpdates")[0]["allowed_updates"]
        assert "stopped_message_generation" in allowed
    finally:
        await channel.close()
        await task


class RefusingClient(Client):
    """Refuses group notices, as when the bot is not an administrator."""

    def __init__(self, pinned=None):
        super().__init__()
        self.pinned = pinned

    async def call(self, method, payload=None, **options):
        if method == "sendMessage" and payload and "ephemeral_message_parameters" in payload:
            self.calls.append((method, payload))
            raise TelegramError("Bad Request: not enough rights", error_code=400)
        if method == "getChat":
            self.calls.append((method, payload))
            return {"id": payload["chat_id"], "type": "private", "pinned_message": self.pinned}
        return await super().call(method, payload, **options)


@pytest.mark.asyncio
async def test_a_refused_notice_falls_back_to_the_private_chat_when_asked():
    client = RefusingClient()
    channel = faked(client)
    app = AgentChat(channels=[channel])
    await channel._load_bot_identity()
    group = channel._normalize(group_update())
    with pytest.raises(TelegramError):
        await app.send_ephemeral(channel, group, "30", "Only you")
    notice = await app.send_ephemeral(channel, group, "30", "Only you", fallback="dm")
    request = client.method_calls("sendMessage")[-1]
    assert request["chat_id"] == 30 and "reply_parameters" not in request
    assert notice.conversation_id == "telegram:99:30"
    private = channel._normalize(telegram_update())
    await app.send_ephemeral(channel, private, "31", "Elsewhere", fallback="dm")
    assert client.method_calls("sendMessage")[-1]["chat_id"] == 31


@pytest.mark.asyncio
async def test_home_message_is_pinned_then_edited_and_found_again_after_a_restart():
    client = RefusingClient()
    channel = faked(client)
    await channel._load_bot_identity()
    home = await channel.publish_home("30", "Welcome")
    assert client.method_calls("sendMessage")[-1]["text"] == "Welcome"
    assert client.method_calls("pinChatMessage") == [
        {"chat_id": 30, "message_id": 101, "disable_notification": True}]
    again = await channel.publish_home("30", "Welcome back")
    assert again.id == home.id
    assert client.method_calls("editMessageText")[-1]["text"] == "Welcome back"
    assert len(client.method_calls("getChat")) == 1

    restarted = RefusingClient(pinned={"message_id": 101, "from": {"id": 99}})
    channel = faked(restarted)
    await channel._load_bot_identity()
    await channel.publish_home("30", "Still here")
    assert restarted.method_calls("sendMessage") == []
    assert restarted.method_calls("editMessageText")[-1]["message_id"] == 101


@pytest.mark.asyncio
async def test_a_chat_form_offers_buttons_for_choices_and_takes_text_replies():
    client = Client()
    channel = faked(client)
    app = AgentChat(channels=[channel])
    submitted = []

    @app.on_message
    async def respond(context):
        await context.open_form({"callback_id": "team", "fields": [
            {"id": "plan", "label": "Which plan?", "type": "choice", "options": ["Free", "Pro"]},
            {"id": "name", "label": "Team name?"},
        ]})

    @channel.on_view("team")
    async def created(payload):
        submitted.append(payload["values"])

    client.batches = [[telegram_update(1, text="start")]]
    task = await running(channel, client)
    try:
        await until(lambda: client.method_calls("sendMessage"))
        question = client.method_calls("sendMessage")[0]
        assert question["text"].startswith("Which plan?")
        buttons = [b for row in question["reply_markup"]["inline_keyboard"] for b in row]
        pro = next(button for button in buttons if button["text"] == "Pro")
        channel._has_admitted_all([callback_update(data=pro["callback_data"], update_id=3)])
        await until(lambda: len(client.method_calls("sendMessage")) == 2)
        assert client.method_calls("sendMessage")[1]["text"] == "Team name?"
        # The pressed question loses its buttons and shows the answer.
        assert "Pro" in client.method_calls("editMessageText")[-1]["text"]
        channel._has_admitted_all([telegram_update(4, text="Rockets", message_id=102)])
        await until(lambda: submitted)
        assert submitted == [{"plan": "Pro", "name": "Rockets"}]
    finally:
        await channel.close()
        await task


class OldMessageClient(Client):
    """Refuses to delete, as for a message older than 48 hours."""

    async def call(self, method, payload=None, **options):
        if method == "deleteMessage":
            self.calls.append((method, payload))
            raise TelegramError("Bad Request: message can't be deleted", error_code=400)
        return await super().call(method, payload, **options)


@pytest.mark.asyncio
async def test_a_message_that_cannot_be_deleted_is_edited_when_asked():
    client = OldMessageClient()
    channel = faked(client)
    app = AgentChat(channels=[channel])
    await channel._load_bot_identity()
    sent = await channel.reply(channel._normalize(telegram_update()), "Old news")
    assert await app.delete(channel, sent) == "failed"
    assert await app.delete(channel, sent, fallback="edit") == "edited"
    edit = client.method_calls("editMessageText")[-1]
    assert (edit["text"], edit["chat_id"], edit["message_id"]) == (
        "(This message was removed.)", 30, 101)
    edited = await app.edit(channel, sent, "Corrected")
    assert edited.text == "Corrected" and edited.id == sent.id
    with pytest.raises(ValueError):
        await app.delete(channel, sent, fallback="hide")
