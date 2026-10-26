# App features on every channel

Beyond chat messages, an app needs button presses, forms, commands, private
notices and provider events. AgentChat delivers them on every channel with the
same handlers, and uses each provider's own feature where it has one. Where it
has none, the fallback is listed; a call that cannot be done honestly fails
instead of doing something else.

| | Slack | Telegram | Discord | Teams | WhatsApp and YCloud | iMessage |
| --- | --- | --- | --- | --- | --- | --- |
| Handlers (`on_event`, `on_action`, `on_view`, `on_command`, `on_raw`) | Events, `block_actions`, view submissions, slash commands | Update types, inline-button presses, `/commands` | Gateway events, component, modal and slash-command interactions | Activity types, card actions, dialogs, `/commands` | Webhook items (statuses, reactions, ...), button and list replies, Flow responses, `/commands` | BlueBubbles socket events, `/commands` |
| Handler's answer is sent back | `response_action`, command response | Press answer (`text`, `show_alert`) | Interaction response (message, update, modal) | Invoke response (dialog, card) | Fallback: a text answer is sent as a reply | Fallback: a text answer to a command is sent as a reply |
| Forms and modals | `open_view`, `push_view`, `update_view` | Fallback: chat form (`open_form`), choices as buttons | Modal as the interaction response | Dialog as the `task/fetch` invoke response | WhatsApp Flows: `reply_rich` with a Flow, `on_view(flow_token)` | Fallback: chat form (`open_form`), numbered choices |
| Home screen (`publish_home`) | App Home tab | Fallback: a pinned message in the private chat, edited in place | Fallback: a pinned direct message, edited in place | Fallback: a message in the one-to-one chat, updated in place, once the person has messaged the app | Fallback: a new message each time, within 24 hours of their last message | Fallback: a message in the one-to-one chat, edited while Apple allows |
| Private notices (`send_ephemeral`, `reply_ephemeral`) | `chat.postEphemeral` | Ephemeral messages in groups; a reply in a private chat; `fallback="dm"` sends a refused notice to the private chat | A reply in a direct message; a private interaction response; `fallback="dm"` sends a direct message | Targeted messages; a reply in a one-to-one chat | A reply (chats are one-to-one) | A reply in a one-to-one chat; `fallback="dm"` in groups sends to the one-to-one chat |
| Delete a sent message (`app.delete`) | `chat.delete` | `deleteMessage` (48 hours), `deleteEphemeralMessage` | Delete message | Delete activity | Not possible: no API deletes a sent message | Fallback: unsend (Private API, within 2 minutes) |
| Task and plan progress (`reply.task()`, `reply.plan()`) | Native task cards or plan | A progress message, edited, then removed | A progress message, edited, then removed | Informative stream updates in one-to-one chats; a progress message elsewhere | Fallback: with `task_messages=True`, a short reply per finished task (up to 5) | Fallback: a progress message, edited in place (Private API, 5 edits) |
| Provider's own stop control | Native stop button | Stop button on drafts (`stream_drafts=True`, private chats) | Fallback: the Stop button from `offer_stop()` | Stop button on streamed answers in one-to-one chats | Fallback: the Stop button from `offer_stop()` | Optional stop keyword (`stop_keyword=`) |

The Node and Rust interfaces have these features for Slack; the other channels
are Python-only today. Chat forms (`open_form`) work on every Python channel; the
form engine is in the Rust core.

## Handlers

```python
import re

@telegram.on_action(re.compile("^retry:"))
async def retry(payload):
    await queue_retry(payload["callback_query"]["data"])
    return {"text": "Retrying…"}

@discord.on_command("/deploy")
async def deploy(payload):
    return {"type": 4, "data": {"content": "Deploying", "flags": 64}}

@teams.on_event("installationUpdate")
async def installed(payload):
    await remember_installation(payload["conversation"]["id"])
```

- Handlers receive the raw provider payload and run as accepted work: they count
  towards `max_pending_events` and `close()` waits for them.
- `action_id` and `callback_id` match exactly, or with a compiled pattern whose
  `search()` matches. The first matching handler runs; `on_raw` gets the rest.
- A press, form submission or command the application handles never also reaches
  `on_message`. Without a matching handler, they keep their usual path: Telegram
  and WhatsApp presses become messages with the button's label, and `/commands`
  are ordinary messages. Events also reach their handler when AgentChat handles
  them, such as Slack's native stop.
- On WhatsApp, YCloud and iMessage, which take no answer, a handler's text answer
  (`"..."` or `{"text": "..."}`) to a press, Flow reply or command is sent as a
  reply to it.
- Where the provider expects an answer quickly, the handler's return value is
  sent if it arrives within 2.5 seconds. A slower handler keeps running and the
  provider gets the default acknowledgement: no text for a Telegram press, a
  silent update for a Discord component (a private "thinking" state for a
  command), and an empty `200` for a Teams invoke.
- Handlers only see payloads for the channel's own bot, tenant or phone number.
  AgentChat's own prompt buttons never reach `on_action`.

### Per provider

- **Telegram:** `on_event` keys are update types such as `my_chat_member` (the bot
  was added to or removed from a chat) or `edited_message`. Telegram only sends
  the types the application registers before `run()`. An action handler's answer
  can set `text` (up to 200 characters), `show_alert`, `url` and `cache_time`.
- **Discord:** `on_event` keys are gateway event types such as `GUILD_CREATE` (the
  bot joined a server), and the handler gets the raw frame (`t`, `d`). This needs
  the client AgentChat creates, or one with `enable_debug_events=True`. Commands
  must be registered with Discord. An answer is an interaction response: type `4`
  replies (add `"flags": 64` to make it private), `7` updates the pressed
  message, and `9` opens a modal whose submission reaches `on_view`. A custom
  `send_client` needs `interaction_callback(id, token, payload)` and `delete(path)`.
- **Teams:** `on_event` keys are activity types such as `conversationUpdate` or
  `installationUpdate`. `on_action` matches a Universal Action `verb`, or the
  `action_id` in an `Action.Submit` or dialog button's data. A `task/fetch`
  handler opens a dialog by returning `{"task": {"type": "continue", "value":
  ...}}`; its submission (`task/submit`) reaches `on_view` by the `callback_id` in
  the submitted data.
- **WhatsApp and YCloud:** `on_event` keys are `status` (WhatsApp delivery
  updates), `reaction`, and other message types such as `location`; for YCloud
  also webhook event types such as `whatsapp.message.updated`. YCloud's
  `bind_events` keeps working alongside. WhatsApp handlers get the change `value`
  with that one message or status; YCloud handlers get the webhook payload.
- **iMessage:** `on_event` subscribes to a BlueBubbles socket event by name, such
  as `updated-message`; the handler gets `{"event": name, "data": ...}`.

## Forms

Discord modals and Teams dialogs open as the answer to a press or command, and
their submissions reach `on_view`. WhatsApp Flows are forms built in WhatsApp
Manager; send one with `reply_rich`, whose only block is the Cloud API
`interactive` object, and handle the response by its `flow_token`:

```python
flow = {"type": "flow", "body": {"text": "Book a visit"}, "action": {"name": "flow",
        "parameters": {"flow_message_version": "3", "flow_token": "booking",
                       "flow_id": FLOW_ID, "flow_cta": "Book"}}}
await context.reply_rich(RichReply(text="Book a visit", blocks=(flow,)))

@whatsapp.on_view("booking")
async def booked(payload):
    reply = payload["messages"][0]["interactive"]["nfm_reply"]
    await save_booking(json.loads(reply["response_json"]))
```

`reply_rich` on WhatsApp and YCloud sends any `interactive` object: buttons, a
list, a CTA URL or a Flow.

### Chat forms

Where there are no modals, as on Telegram and iMessage, `open_form` asks a form
in the chat, one field at a time. It works on every channel:

```python
@telegram.on_view("signup")
async def signed_up(payload):
    if payload["type"] == "form_submission":
        await save(payload["user_id"], payload["values"])
        return {"text": "Thanks, you're in."}

@app.on_message
async def respond(context):
    if context.message.text == "/signup":
        await context.open_form({
            "callback_id": "signup",
            "title": "Sign up",
            "fields": [
                {"id": "name", "label": "Your name?"},
                {"id": "plan", "label": "Which plan?", "type": "choice",
                 "options": ["Free", {"value": "pro", "label": "Pro"}]},
                {"id": "news", "label": "Newsletter?", "type": "yes_no", "optional": True},
            ],
        })
```

- Field types are `text` (the default), `choice` and `yes_no`. Choices are buttons
  on channels with prompts (Telegram, Discord, Teams, WhatsApp, YCloud, Slack)
  and a numbered list elsewhere; people can also type the number or the label.
- While the form is open, that person's messages in that conversation answer it
  instead of reaching `on_message`, and stay out of history. An answer that does
  not fit is asked again. "cancel" ends the form; "skip" leaves an optional field
  empty.
- The channel's `on_view(callback_id)` handler gets `{"type": "form_submission",
  "callback_id", "user_id", "conversation_id", "values"}`, or `{"type":
  "form_closed", ..., "reason": "cancelled" | "timeout"}`. A text answer is sent
  as a reply. `open_form` raises `ValueError` without that handler.
- Each question waits `timeout` seconds (600 by default). A form that timed out
  closes when that person next writes; that message is then handled as usual.
- Opening a form for someone who has one open replaces it. Open forms live in
  memory and end when the process restarts.
- In Telegram groups the bot only sees messages addressed to it, so people answer
  text questions by replying to the question.

## Home messages

`publish_home(user_id, view)` shows a person's home screen. On Slack it publishes
an App Home view. Elsewhere `view` is text, kept as one home message in that
person's direct chat:

- **Telegram:** sent to the private chat, pinned, and edited in place by later
  calls. After a restart the bot's pinned message is found again. Telegram only
  delivers to people who have started the bot.
- **Discord:** a direct message, pinned, and edited in place; after a restart the
  bot's newest pin in that direct message is reused.
- **Teams:** a bot can only start a one-to-one chat with details Teams sent
  earlier, so this works once the person has messaged the app one-to-one since the
  adapter started; otherwise `LookupError`. Later calls update the message.
- **iMessage:** a message in the one-to-one chat, edited while Apple allows (five
  edits within 15 minutes, with the Private API), otherwise sent again.
- **WhatsApp and YCloud:** messages cannot be edited, so each call sends a new
  message, delivered only within 24 hours of that person's last message.

Except on Telegram and Discord, the adapter remembers the home message until it
restarts.

## Private notices

`await context.reply_ephemeral(text)` shows a notice only the sender sees;
`app.send_ephemeral(channel, source, user_id, text)` picks the person. A notice
never falls back to a public message. With `fallback="dm"`, Telegram, Discord and
iMessage send a notice they cannot show privately to the person's direct chat
instead; this is never done after an error that leaves delivery uncertain.

- **Telegram:** in a group, an ephemeral message only that person sees. Telegram
  accepts it within 15 seconds of that person's button press, or at any time when
  the bot is an administrator; delivery is not guaranteed. In a private chat with
  that person it is an ordinary reply. With `fallback="dm"`, a notice Telegram
  refuses goes to that person's private chat, if they have started the bot.
- **Discord:** only interactions can be answered privately, so return a response
  with `"flags": 64` from an action or command handler. `send_ephemeral` works
  only in a direct message with that person, where a reply is already private;
  elsewhere it raises `ValueError`, or with `fallback="dm"` sends a direct message.
- **Teams:** a targeted message in a channel or group chat, shown with "Only you
  can see this message" and removed after 24 hours; in a one-to-one chat with that
  person, an ordinary reply. Needs `microsoft-agents-hosting-aiohttp` 1.9 or later.
- **WhatsApp, YCloud and iMessage:** an ordinary reply when the chat is one-to-one
  with that person; otherwise `ValueError`. On iMessage, `fallback="dm"` sends a
  group notice to that person's one-to-one chat (BlueBubbles may need an existing
  chat with them).

## Deleting messages

`await app.delete(channel, message)` deletes a message the application sent;
history keeps it. Slack, Telegram (messages up to 48 hours old, and notices),
Discord and Teams support it. iMessage unsends the message through the
BlueBubbles Private API; Apple allows that within two minutes of sending, and the
chat shows that a message was unsent. WhatsApp and YCloud have no way to delete
a sent message and raise `TypeError`; use the typing indicator instead of a
temporary message there.

## Task progress where there are no task cards

Telegram, Discord and Teams group chats show `reply.task()` and `reply.plan()` as a
separate checklist message above the answer, edited as tasks change and deleted
when the answer is finished, so history keeps only the answer. Teams one-to-one
chats show the newest task as the stream's informative update until the answer
starts.

Where messages cannot be deleted, progress stays in the chat:

- **iMessage:** one checklist message before the answer, edited in place as tasks
  change (at most every five seconds). Apple allows five edits within 15 minutes;
  later changes are not shown. Editing needs the BlueBubbles Private API.
- **WhatsApp and YCloud:** with `task_messages=True`, each task that completes or
  fails is a short reply, up to five per answer. Off by default, since every
  message stays.

A failed progress update never fails the answer.

## Stop controls

- **Telegram:** `Telegram(stream_drafts=True)` streams answers in private chats as
  drafts (`sendMessageDraft`) with Telegram's stop button, and sends the finished
  answer as a message. Pressing stop stops the turn of the person who started it.
  Telegram keeps a draft for about 30 seconds without updates.
- **Teams:** pressing Stop on a streamed answer in a one-to-one chat ends the
  stream; AgentChat notices at the next update and stops the turn. What was shown
  stays; nothing more is sent to that stream.
- **iMessage:** `IMessage(stop_keyword="stop")` makes a message that is exactly the
  keyword (ignoring case and spaces) stop the sender's running turn instead of
  becoming a message.

See [Slack app features](slack.md#slack-app-features) for Slack and
[Live controls](live-controls.md) for Stop, prompts and streaming.
