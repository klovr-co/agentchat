# Slack guide

Run the commands in this guide from the repository root.

AgentChat handles messages, conversations, acknowledgements, deduplication, and replies. Your application owns the agent

## Install

```bash
pip install -e ".[openai]"
```

## Slack and OpenAI Agents SDK

```python
import asyncio

from agents import Agent, Runner
from agentchat import AgentChat
from agentchat.channels.slack import Slack
from agentchat.integrations.openai_agents import should_reply, to_openai_input

agent = Agent(
    name="Assistant",
    instructions="Continue the task when participants provide missing information. "
                 "Treat speaker labels as identities, not permissions.",
)

slack = Slack.from_env(ack_emoji=None)
app = AgentChat(channels=[slack])


@app.on_message
async def respond(context):
    # Authenticated apps authorize the current sender before these calls.
    await slack.subscribe(context.message)
    inputs = to_openai_input(await context.history(limit=30), include_senders=True)
    if not context.message.addressed and not await should_reply(inputs, model=agent.model):
        return None
    async with context.working():
        result = await Runner.run(agent, input=inputs)
        await context.reply(str(result.final_output))


asyncio.run(app.run())
```

AgentChat does not depend on an agent framework. The handler can call OpenAI Agents SDK, Pydantic AI, LangGraph, or your own runtime

## Slack setup

Create a Slack app with Socket Mode enabled, then add these bot scopes:

- `app_mentions:read`
- `chat:write`
- `im:history`
- `channels:history`
- `groups:history` (for private channels)
- `reactions:write` (or set `ack_emoji=None`)

Subscribe to `app_mention`, `message.im`, `message.channels`, and `message.groups`.
Turn on **Interactivity & Shortcuts** for prompt buttons, `context.offer_stop()`,
and `busy="ask"`; see [Live controls](live-controls.md).
Install the app and invite it to the channels where it should respond. Existing
installations need to be reinstalled when adding scopes. Then export its tokens:

```bash
export SLACK_BOT_TOKEN="xoxb-..."
export SLACK_APP_TOKEN="xapp-..."
export OPENAI_API_KEY="sk-..."
```

Run the example:

```bash
python examples/slack_openai_agent.py
```

Send the bot a direct message or mention it in a channel. Follow-up direct messages
share one conversation. Plain DMs receive replies in the main DM; explicit DM
thread replies stay in that thread. Channel mentions receive replies under the
original thread root.

Call `await slack.subscribe(context.message)` after your application accepts a
channel conversation to receive its subsequent replies without another mention.
For an authenticated application, check the sender's access **before** subscribing.
Unrelated channel messages and threads are ignored. Other people's mentions remain
in the message text; only the bot's own mention is removed.

Subscriptions default to memory, bounded to the latest 1,000 threads. To retain
them across restarts, pass `thread_subscriptions=` with an object implementing
`async contains(conversation_id) -> bool` and `async add(conversation_id) -> None`.
The application owns that store's expiry policy. Subscribing does not authorize
other participants; check each incoming sender independently.

## Working indicators

Use Slack’s native “Your Agent is working…” indicator after accepting a request:

```python
async with context.working() as activity:
    result = await run_agent()
    await activity.update("is checking the results…")
    await context.reply(result)
```

The helper refreshes every 60 seconds and clears on exit, including failures and
cancellation. Status updates stay outside conversation history and create no chat
messages. Status API failures are logged without preventing the agent’s answer.
Slack expires an abandoned indicator after two minutes. Set `ack_emoji=None` on
`Slack(...)` or `Slack.from_env(...)` to use the indicator without eyes reactions.
The SDK keeps existing reaction defaults for backward compatibility.

For durable workers, use `await app.set_status(channel, source, "is working…")`
(or `await context.set_status(...)`) and clear with an empty string. This one-shot
API propagates provider failures; setting the same status is safe to retry. Your
worker owns refresh/recovery from persisted task state. Posting a bot message
clears Slack’s indicator, so refresh it again if more work remains. A `working()`
context is for one in-process task, not a replacement for durable orchestration.

Slack automatically prefixes the app’s name. `assistant.threads.setStatus` accepts
`chat:write` (existing `assistant:write` also works). The indicator targets the
existing reply thread, including explicitly threaded DMs. Plain top-level DMs and
channels without status support return `False` rather than opening an unexpected
thread. This feature does not move conversations into Slack’s new agent-session
channels or require a new installation permission. To report status through agent
sessions instead, see [Agent sessions](#agent-sessions-and-the-native-stop-button).

## Slack app features

Slack sends Socket Mode payloads to whichever connection is open, so an app can't
run AgentChat for messages and Slack Bolt for everything else on one app token.
AgentChat delivers every payload: messages, prompt answers and Slack's native
stop become AgentChat input, and everything else goes to your handlers.

```python
import re

from agentchat import AgentChat
from agentchat.channels.slack import Slack

slack = Slack.from_env(ack_emoji=None)
app = AgentChat(channels=[slack])


@slack.on_event("app_home_opened")
async def home(payload):
    event = payload["event"]
    if event.get("tab") == "home":
        await slack.publish_home(event["user"], {"type": "home", "blocks": [...]})


@slack.on_action("settings")
async def open_settings(payload):
    # trigger_id expires three seconds after Slack sends it: open the modal first.
    await slack.open_view(payload["trigger_id"], settings_modal())


@slack.on_action(re.compile(r"^retry_"))
async def retry(payload):
    ...


@slack.on_view("settings")
async def save_settings(payload):
    values = payload["view"]["state"]["values"]
    if not valid(values):
        return {"response_action": "errors", "errors": {"name": "Enter a name"}}
    await save(payload["user"]["id"], values)
    return None  # Close the modal.


@slack.on_command("/deploy")
async def deploy(payload):
    return {"response_type": "ephemeral", "text": f"Deploying {payload['text']}"}


@slack.on_event("member_joined_channel")
async def joined(payload):
    if payload["event"]["user"] == my_bot_user_id:
        await refresh_allowed_channels()
```

| Decorator | Receives | Return value |
| --- | --- | --- |
| `on_event(type)` | Events API events AgentChat does not turn into a message, as the whole envelope (`payload["event"]`, `payload["team_id"]`, `payload["authorizations"]`) | Ignored |
| `on_action(action_id)` | `block_actions` payloads (`trigger_id`, `user`, `channel`, `message`, `container`, `actions`, `response_url`) | Ignored |
| `on_view(callback_id)` | `view_submission` and `view_closed` (`trigger_id`, `user`, `view`) | For a submission, the acknowledgement: `response_action` `errors`, `update`, `push` or `clear` |
| `on_command(command)` | Slash commands (`command`, `text`, `user_id`, `channel_id`, `trigger_id`, `response_url`) | The response, such as `{"text": ...}` |
| `on_raw` | Anything no other handler matched: shortcuts, message shortcuts, options requests | The acknowledgement, for options requests |

`action_id` and `callback_id` match exactly, or with a compiled pattern whose
`search()` matches, as in Bolt. The first matching handler runs. Handlers receive
the raw payload and run on the event loop of `app.run()`.

Acknowledgements:

- Slack needs one within three seconds. Events, button presses and shortcuts are
  acknowledged as soon as they arrive, then the handler runs.
- View submissions, options requests and slash commands can carry a response,
  so AgentChat waits for the handler and sends its return value as the
  acknowledgement. A handler that takes longer than 2.5 seconds gets an empty
  acknowledgement (Slack closes the modal) and keeps running; `close()` waits for
  it like other accepted work.
- Payloads without a handler are acknowledged and ignored, as before.
- Payloads from another workspace are ignored. With `workspace_id=` set, or after
  `run()` authenticates, the team must match; an interactive payload without a
  team is dropped.
- AgentChat's own prompt buttons (Stop, `ask()`, Queue/Steer) never reach
  `on_action`. Messages AgentChat ignores, such as other bots' messages or
  unsubscribed thread replies, reach `on_event("message")`.

### Modals and App Home

`await slack.open_view(trigger_id, view)` opens a modal; `push_view(trigger_id,
view)` pushes one onto an open modal; `update_view(view, view_id=... or
external_id=..., hash=None)` replaces one; `publish_home(user_id, view,
hash=None)` publishes a user's Home tab (the view's `type` must be `"home"`).
Each returns the view Slack sends back and raises Slack's error, for example
`expired_trigger_id`, without retrying.

### Private notices

```python
await context.reply_ephemeral("Only you can see this.")
await context.reply_ephemeral("Your report is ready.", user_id="U123", blocks=[...])
await app.send_ephemeral(slack, source, "U123", "Only you can see this.")
```

Ephemeral messages use `chat.postEphemeral` in the source thread and are not
added to conversation history. Slack delivers them only if the person is active in
Slack and a member of the channel, does not keep them across reloads or devices,
and shows one in a thread only when the thread is already active. Treat them as
notices, not as the only copy of something important. Failures raise; a notice
is never posted publicly instead. Channels without private notices raise
`TypeError`: Discord only has ephemeral responses to interactions, and Teams and
the other channels have none, so only Slack supports this.

### Deleting messages

`await app.delete(slack, message)` deletes a message the app sent, such as a
temporary "Working…" reply, with `chat.delete`. History keeps it. Ephemeral
notices cannot be deleted through the Web API and raise `ValueError`. Other
channels raise `TypeError` for now.

### Agent sessions and the native stop button

Slack shows a stop button while an agent session is `processing`, if the app
subscribes to the `agent_session_stopped` event. AgentChat turns that event into
a stop request for the thread's conversation: it stops the running turn when the
person who started the turn pressed it (like the default Stop button), clears the
thread's status, and also passes the event to `on_event("agent_session_stopped")`.

Status updates use `assistant.threads.setStatus` by default; Slack maps its
non-empty status to an agent session in `processing` and an empty one to
`active`. With `Slack(..., agent_sessions=True)`, `set_status()` and `working()`
call `agents.sessions.setStatus` directly (`processing` while working, `active`
when cleared). That method takes no custom text, so Slack shows its own loading
text. It needs an app declared as an agent in the app settings and returns
`feature_disabled` on workspaces without agent sessions, so it is off by default.
`chat.startStream` also starts a session, and `chat.stopStream` sets it `active`.

### Host-owned HTTP sources

A `SlackEventSource` delivers Events API envelopes, interactive payloads (the
decoded `payload` form field) and slash commands to the receiver. When
`agentchat.channels.slack.events.responds(payload)` is true (view submissions,
options requests and slash commands), await the receiver and return its value as
the HTTP response body; it returns within `events.ACK_DEADLINE` (2.5) seconds.
Acknowledge everything else first, as before. Rust hosts call
`slack.handle_payload(&payload)`, which returns the message for the runtime and
the acknowledgement body.

### Node and Rust

```ts
const slack = Slack.fromEnv({ agentSessions: false });
slack.onAction("settings", async (payload) => {
  await slack.openView(payload.trigger_id as string, settingsModal());
});
slack.onView("settings", () => ({ response_action: "clear" }));
await context.replyEphemeral("Only you can see this.");
```

The Node `Slack` has `onEvent`, `onAction` (string or `RegExp`), `onView`,
`onCommand`, `onRaw`, `openView`, `pushView`, `updateView`, `publishHome`,
`sendEphemeral` and `delete`; the `AgentChat` it belongs to must exist first. In
Rust, `Slack` has `on_event`, `on_action` (`&str` or `Regex`), `on_view`,
`on_command` and `on_raw` with `async` handlers returning
`Result<Option<Value>>`, the same view methods, `send_ephemeral` and `delete`.

### Manifest scopes and events

| Feature | Bot scopes | Events and settings |
| --- | --- | --- |
| Messages | `app_mentions:read`, `chat:write`, `im:history`, `channels:history`, `groups:history` | `app_mention`, `message.im`, `message.channels`, `message.groups` |
| Buttons, modals, shortcuts, options | — | **Interactivity & Shortcuts** on |
| Slash commands | `commands` | Each command under **Slash Commands** |
| Shortcuts | `commands` | Each shortcut under **Interactivity & Shortcuts** |
| Modals (`views.open`, `push`, `update`) | None | Interactivity, for the `trigger_id` |
| App Home (`views.publish`) | None | **App Home** tab on; `app_home_opened` |
| Private notices (`chat.postEphemeral`) | `chat:write` | — |
| Deleting (`chat.delete`) | `chat:write` (only the app's own messages) | — |
| Streams with tasks and plans | `chat:write` | — |
| Working status | `chat:write` | — |
| Agent sessions (`agent_sessions=True`) | `chat:write`; declaring the app as an agent adds `assistant:write` | — |
| Native stop button | `chat:write` | `agent_session_stopped` |
| Following invitations | `channels:read`, `groups:read`, `mpim:read` | `member_joined_channel` |

Reinstall the app after adding scopes.

## Moving from Slack Bolt

| Slack Bolt | AgentChat |
| --- | --- |
| `App(token=..., ...)` with `SocketModeHandler(app, app_token)` | `Slack(bot_token=..., app_token=...)` in `AgentChat(channels=[slack])`; `await app.run()` |
| `@app.event("app_mention")`, `@app.event("message")` for the agent | `@app.on_message`; DMs, mentions and subscribed threads arrive normalized and deduplicated |
| `@app.event("app_home_opened")` and other events | `@slack.on_event("app_home_opened")`; the event is `payload["event"]` |
| `@app.action("id")`, `@app.action(re.compile(...))` | `@slack.on_action("id")`, `@slack.on_action(re.compile(...))` |
| `@app.view("callback_id")` with `ack(response_action=...)` | `@slack.on_view("callback_id")` returning `{"response_action": ...}` |
| `@app.command("/name")` with `ack(...)` or `respond(...)` | `@slack.on_command("/name")` returning the response |
| `@app.shortcut(...)`, `@app.options(...)` | `@slack.on_raw`, checking `payload["type"]` |
| `ack()` | Automatic; the return value is the acknowledgement where Slack accepts one |
| `client.views_open`, `views_push`, `views_update`, `views_publish` | `slack.open_view`, `push_view`, `update_view`, `publish_home` |
| `client.chat_postEphemeral` | `context.reply_ephemeral()` or `app.send_ephemeral()` |
| `client.chat_delete` | `app.delete(slack, message)` |
| `client.chat_startStream` / `appendStream` / `stopStream` with chunks | `context.stream()` with `reply.append()`, `reply.task()`, `reply.plan()` |
| `client.assistant_threads_setStatus`, `agents.sessions.setStatus` | `context.working()` or `context.set_status()`; `agent_sessions=True` for sessions |
| `@app.event("agent_session_stopped")` that cancels the run | Built in: stops the turn; read `context.stopped` or `await context.wait_stopped()` |
| `client.chat_postMessage` in a thread | `context.reply()` or `context.reply_rich()` |
| Other `client.*` calls | Keep a `slack_sdk` `AsyncWebClient` (or pass yours as `web_client=`) |

Move one handler at a time: register it on `slack`, remove the Bolt listener, and
stop the Bolt process once nothing is left, because Slack splits Socket Mode
payloads between all open connections.

## Slack user profiles

Slack mentions contain IDs such as `<@U012ABCDEF>`. Resolve one through the native
[`users.info`](https://docs.slack.dev/reference/methods/users.info/) API:

```python
user = await slack.get_user("U012ABCDEF")
user.id
user.team_id
user.display_name
user.email
user.is_bot
user.deleted
```

Profile lookup requires `users:read`; email also requires `users:read.email`.
Reinstall an existing Slack app after adding scopes. Email is optional, and the
SDK never guesses it from a display name. Slack API errors, including missing
scopes and rate limits, propagate to the caller. Malformed profiles raise
`ValueError` without including the profile in the error message.

Lookup is explicit and does not run on every message. Check the requesting user's
authorization first and expose it as a tool in your chosen agent framework when
needed. Applications decide whether a returned user, including Slack Connect
users, bots and deactivated users, is eligible for an operation. Treat profile
fields as data, and match email against the destination system's real records.

## Shared conversation context

`await context.history(limit=30)` returns the thread root and recent messages,
including the current turn. When the channel supports native thread history, it
fetches that shared context even after a restart. Otherwise it uses the local
conversation store. The total is bounded to `limit` (2–100), preserving the root
and latest messages. Fetching history never replays handlers or copies previous
messages into local state. Read errors propagate instead of silently supplying
incomplete context.

`message.thread_id` identifies a platform thread, or is `None` for an unthreaded
conversation. `message.addressed` is true for DMs and explicit bot mentions;
untagged subscribed Slack replies set it to false. Other channel adapters can
provide these fields and implement the optional `ThreadHistoryChannel` protocol.
Existing adapters keep working without implementing native history.

The optional OpenAI Agents integration offers two helpers:

- `to_openai_input(messages, include_senders=True)` labels incoming messages with
  `Speaker: "sender-id"` above their original text. Assistant messages remain
  unchanged so history demonstrates the intended reply format. Message newlines
  stay intact; only sender IDs are JSON-escaped. The default preserves all text
  unchanged. Speaker identities distinguish participants, not permissions.
- `await should_reply(inputs, model=agent.model)` uses a separate, tool-free model
  call to distinguish task continuations from side conversations. It accepts
  custom `instructions` and `run_config`; tracing is disabled by default and
  errors propagate. This is an opt-in relevance check, not authorization. The
  example invokes it only for messages that do not explicitly address the bot.

Authorize the current sender **before** fetching history or running a reply
filter. The SDK core does not invoke models or depend on OpenAI Agents; other
frameworks can consume the same normalized messages and implement their own filter.

### Direct Slack history access

`await slack.thread_history(message, limit=50)` reads the thread root and recent
replies before the current message from Slack's `conversations.replies` API.
Returned `Message` objects preserve sender IDs; only this bot's messages have the
assistant role. This lets a new process recover a shared conversation without
mixing users' credentials or private agent/tool state.

Authorize the current sender before fetching history. History is context, not
authorization to execute earlier participants' requests. Keep speaker identities
when passing it to an agent. Lookup uses the installed token's history scopes;
Slack API errors propagate. Pagination is bounded to 500 messages and fails on
incomplete results. The returned context retains the root plus the latest replies
within `limit` (2–100); a new root message has no earlier thread history.

## Core interface

```python
@app.on_message
async def respond(context):
    return await my_agent(context)
```

The context exposes the normalized message, sender, and conversation:

```python
context.message
context.sender
context.conversation

history = await context.history(limit=30)
await context.reply("Hello")
```

Use `context.conversation.history(limit=30)` when you specifically want only the
messages already held in your application's state store.

Incoming messages are acknowledged, deduplicated, and serialized by conversation before the handler runs. Returning a string posts it to the originating Slack DM or thread

Slack replies display the full response by default using expanded section blocks.
Long text replies are split across sections within a single Slack message, with
code fences balanced across section boundaries. The original text remains the
notification and accessibility fallback. Text replies that require more than Slack's
50-block limit raise `ValueError` before sending; split those into separate replies.

## Incoming attachments

`Message.attachments` holds a tuple of `Attachment` references with a provider
file ID and optional name, media type and byte size. Slack accepts `file_share`
events and file-only DMs, mentions and subscribed thread replies. Native thread
history also retains attachment references, including messages without text.
The existing workspace, bot-message and thread-subscription checks still apply.
Normalizing an event never downloads files or stores private file URLs.

After authorizing the sender, explicitly fetch a current-message file:

```python
@app.on_message
async def respond(context):
    for attachment in context.message.attachments:
        file = await context.download_attachment(attachment, max_bytes=10 * 1024 * 1024)
        # Validate file.content, store it, or prepare a multimodal model input.
        # file.name and file.media_type describe the freshly fetched metadata.
    return "Received your attachments."
```

`DownloadedFile` carries the file ID, name, bytes and optional media type. Bytes
are never appended to AgentChat history or downloaded automatically by
`to_openai_input`, which remains a text adapter. Applications own content
validation, image previews, persistence, audio transcription and model-specific
multimodal inputs. Treat file contents as untrusted reference data.

Downloads require the bot's `files:read` scope; reinstall an existing Slack app
after adding it. AgentChat resolves `files.info` by ID, validates the private
Slack host, sends the bot token only to that host, disables redirects and checks
both the declared size and streamed byte count. Errors and cancellation propagate
without automatic retry. The default per-file limit is 10 MB; hosts can pass a
different positive `max_bytes` budget. Slack normalization retains at most five
files per message.

Custom channels can implement `FileDownloadChannel`. The application-level
`app.download_attachment(channel, source, attachment, max_bytes=...)` checks that
the reference belongs to the source message; unsupported channels raise
`TypeError`. Downloads from a historical Slack message can use this API with
that historical message as `source` after the host authorizes access.

Host-owned signed webhooks can reuse
`agentchat.channels.slack.events.slack_attachments(files, limit=5)` for normalization
and `agentchat.channels.slack.files.download_slack_file(attachment, bot_token=..., request=..., max_bytes=...,
before_download=...)` for transport. The async `request(api, payload)` callback
authenticates the Slack metadata call. The optional guard runs before metadata
retrieval, before downloading and before returning bytes, so hosts can recheck
revoked access. The host remains responsible for verifying signed events,
workspace/sender authorization, durable queues and recovering any file references
omitted by a provider event. Event-provided download URLs are never used.

## Rich replies and file uploads

Send application-selected bytes and a rich reply to the accepted conversation:

```python
from agentchat import RichReply, UploadFile

# Your application has authorized this sharing and verified the bytes and URL.
files = await context.upload_files((
    UploadFile(filename="demo.webm", content=verified_video_bytes, title="Working demo"),
))
await context.reply_rich(RichReply(
    text=f"Working demo and PR: {verified_pr_url}",
    blocks=({
        "type": "section",
        "text": {"type": "mrkdwn", "text": f"<{verified_pr_url}|View PR>"},
    },),
))
```

`RichReply` contains fallback `text`, optional `blocks`, and optional `attachments`.
Blocks and attachments must match the receiving provider's format; the Slack
adapter accepts Slack Block Kit and attachment objects. The existing `reply(str)`
API remains available. Custom channels can implement the optional `RichReplyChannel`
and `FileUploadChannel` protocols. Unsupported capabilities raise `TypeError`, so
the host can choose a text or protected-link fallback explicitly.

Slack section blocks, including sections inside attachments, default to
[`expand: true`](https://docs.slack.dev/reference/block-kit/blocks/section-block/)
so readers do not need to click **Show more**. Set `"expand": False` on a section
to opt into Slack's collapsed display. Other block types and explicit expansion
choices are preserved, and the adapter does not modify your input objects.
When a `RichReply` omits blocks, its text is rendered in expanded sections too.

`UploadFile` carries a filename, bytes, and optional title; it does not load paths
or fetch URLs. Uploads return `UploadedFile` receipts with a provider ID and an
optional permalink. They do not append binary content or invented file messages
to conversation history. Rich replies append the confirmed response, like text
replies. Durable hosts can call `app.reply_rich(channel, source, content)` and
`app.upload_files(channel, source, files)` with their saved source message.

Slack file uploads require the bot's `files:write` scope; reinstall an existing app
after adding it. Rich replies use `chat:write`. These APIs expose no new agent
tools and grant no application permissions. The host owns sender authorization,
trusted destination bindings, file selection, content checks, size budgets,
paused-conversation rules, and durable delivery state. Each call makes one attempt;
provider failures propagate and uploads are never retried automatically. The
default Slack client disables automatic retries; an injected client or transport
must also use one-attempt behavior. A timeout may mean Slack accepted a send or
file share, so retain uncertain outcomes rather than blindly retrying them.

Custom Slack webhook or rotating-OAuth adapters can reuse
`agentchat.channels.slack.files.upload_slack_files(files, request=..., channel_id=..., thread_ts=..., before_send=...)`.
The `request(api_method, payload)` callback supplies the host's one-attempt Slack API
transport. The optional async `before_send()` callback rechecks host authorization
before every allocation, byte transfer, and final share. The helper owns upload URL
validation, transfer and receipt verification; it does not require Socket Mode.
`agentchat.channels.slack.rendering.rich_payload(reply)` builds content fields while the host supplies its saved routing.

Durable hosts can also delegate routing and update payloads to
`agentchat.channels.slack.rendering.message_request`. It validates the channel and
timestamps, renders the reply, selects `chat.postMessage` or `chat.update`, and
clears old attachments on updates. It does not send, authenticate or retry:
Posting as a custom username or avatar uses Slack's `chat:write.customize` bot
scope; `username` and `icon_url` apply only to new posts, not updates.

```python
from agentchat import RichReply
from agentchat.channels.slack.rendering import message_request

request = message_request(
    RichReply("Completed", blocks=()),
    channel_id="C01234567",
    thread_ts="1792100000.000",
    message_ts="1792100000.001",  # Omit to post a new reply.
    metadata={"event_type": "saved_delivery", "event_payload": {"id": "one"}},
    fallback_blocks=False,
)
# After the host's durable claim and authorization recheck:
result = await request_once(request["method"], request["payload"])
```

By default, empty `RichReply.blocks` generates readable text sections.
`fallback_blocks=False` preserves explicit empty blocks for plain replacements
or card-only replies. Persist the host's delivery identity before calling its
one-attempt transport, and reconcile uncertain outcomes instead of resending.

## Mirroring web inputs into Slack

Use `app.mirror()` to display an already accepted web input in a bound Slack
conversation, without calling the agent handler or creating another model turn:

```python
from agentchat import Message, Sender

# Authorize the signed-in user and load the session's saved Slack source first.
external = Message(
    id="web:session-123:message-456",  # Stable ID from your saved message
    conversation_id=slack_source.conversation_id,
    channel="web",
    sender=Sender(id=verified_user.id, display_name=verified_user.name),
    text=saved_message.text,
    role="user",
)
await app.mirror(slack, slack_source, external, origin="My app web")
```

Slack shows a bot post labelled “Alice · via My app web”; it does not impersonate
Alice's Slack account. Inputs remain `user` messages in AgentChat state and in
native thread history, including after a restart. Only mirror metadata posted by
the configured bot is recognized. Bot echoes never invoke the handler. Plain-text
blocks, escaped fallback text and disabled unfurls prevent input from creating
Slack mentions. No additional scopes are needed beyond posting/history access.

The application must verify the sender, authorize sharing, and resolve the
original conversation from a trusted session binding. Do not accept destination
IDs or sender identity from browser input. Preserve your own rules for paused
conversations. **Each call attempts one send.** Use a durable outbox with a unique
key per message/chunk, save it with the web input, and mark sends in flight before
posting. Do not automatically retry an ambiguous timeout: Slack may have accepted
the post. A stable `Message.id` alone does not provide provider deduplication.

Inputs are limited to 2,800 characters per call; split larger inputs into ordered,
stable chunks in your outbox. `app.mirror()` appends the sent user message to state
but never invokes a handler or claims an inbound event. If your application owns
history, use `slack.mirror()` directly. Custom webhook adapters can implement the
optional `MirroringChannel` capability and reuse
`agentchat.channels.slack.mirroring.mirror_payload()` for safe Slack content.

## Using an existing agent runner

An application that already owns history, authorization and durable deduplication
can bind directly to the public channel API without a second `AgentChat` state store:

```python
slack = Slack.from_env()

async def receive(channel, message):
    # Verify the sender, claim the message ID and run your existing agent here.
    await slack.subscribe(message)
    await channel.reply(message, "Your response")

slack.bind(receive)
asyncio.run(slack.run())
```

With direct binding, the application owns deduplication and request ordering.
`message.id` identifies the Slack message across retries and duplicate
`app_mention`/`message` deliveries. Metadata includes the original `event_id`,
`team_id`, `channel_id`, `channel_type`, `message_timestamp`, and reply timestamp.

`Slack.run()` checks the bot identity. Pass `workspace_id=` to reject a token or
event from another workspace. `await slack.is_connected()` exposes socket readiness.
The default Socket Mode source acknowledges envelopes before processing in tracked background tasks.
`max_pending_events` defaults to 64; excess envelopes are left unacknowledged for
Slack's retry policy. `await slack.close()` closes the socket, then drains tasks
for up to 20 seconds before cancelling unfinished work. This is bounded in-process
delivery, not a durable queue; applications performing writes need their own journal.

## Composing event sources

`Slack` combines a replaceable incoming event source with shared Slack message
behavior. Existing Socket Mode setup remains:

```python
slack = Slack(bot_token="xoxb-...", app_token="xapp-...")
# Equivalent environment-based convenience:
slack = Slack.from_env()
```

For a host-owned verified webhook queue or another Slack event feed, supply a
`SlackEventSource` instead. No app token or Socket Mode connection is needed:

```python
import os

from agentchat import AgentChat, MessageContext
from agentchat.channels.slack import Slack, SlackEventSource


async def serve(source: SlackEventSource) -> None:
    slack = Slack(
        bot_token=os.environ["SLACK_BOT_TOKEN"],
        event_source=source,
        ack_emoji=None,
    )
    app = AgentChat(channels=[slack])

    @app.on_message
    async def respond(context: MessageContext) -> str:
        # The application authorizes the sender before running agent tools.
        return context.message.text or "Received your attachment."

    try:
        await app.run()
    finally:
        await app.close()
```

Your source implements these operations structurally; no inheritance is required:

- `async run(receiver)`: deliver already authenticated Slack Events API payloads
  through `await receiver(payload)` for the source's lifetime.
- `async close()`: stop intake and release resources, draining or cancelling
  in-flight work according to the source's documented policy; repeated close calls
  must be safe.
- `async is_connected() -> bool`: report that source's readiness.

The receiver takes the same mapping accepted by `slack.handle_event`, including
`team_id` and `event`. Slack verifies bot identity and the configured workspace
before starting the source. Subscriptions, parsing, files, history and replies
are unchanged across sources, and AgentChat continues to own deduplication and
conversation ordering. Outgoing Slack API calls are independently injectable
through `web_client=`. `max_pending_events` configures the default Socket Mode
source; a custom source owns its own concurrency and queue limits.

Supply exactly one of `app_token` and `event_source`. A bare `Slack(bot_token=...)`
without a source is rejected. For an existing host that binds directly to the
channel, supply its source and let `Slack.run()` authenticate before delivering
events through it.

The source owns HTTP signature/timestamp verification and transport acknowledgements.
`handle_event` accepts decoded events; it is not an HTTP signature verifier. Hosts
using durable queues must define when events are acknowledged and how uncertain
outcomes are recovered. The included Socket Mode source remains bounded,
in-process delivery. It owns socket acknowledgements, connection state, pending
event limits and shutdown independently of Slack's message behavior.

See [the source protocol](../python/agentchat/channels/slack/transport.py) and
[the injected-source tests](../tests/channels/slack/test_event_source.py) for a
concrete queue-source composition that reuses the same adapter.

## Development

```bash
python -m pip install -e ".[dev]"
ruff check .
pytest
```

### Applications with their own Slack inbox

`SlackHost` provides the same event routing and message interpretation as the Slack
channel without creating another inbox, credential store, or subscription store.
The application authenticates the incoming event, authorizes each context read,
and owns admission, retries, and persistence.

```python
from agentchat.channels.slack.host import SlackHost

host = SlackHost(
    bot_user_id="UBOT00001",
    workspace_id="TWORK0001",
    exclude_prefixes=("!aside",),  # Optional application policy.
)
event = host.normalize_event(authenticated_payload)
if event is not None:
    context = await host.thread_context(event, authorized_page_request)
```

After deciding to handle a normalized event, call `await host.acknowledge(event,
request)` before slower work such as reading thread context. This best-effort
`reactions.add` uses the event's `message_ts`; it returns `False` when disabled,
timed out, or not accepted.

`authorized_page_request` is an async callback receiving `conversations.replies`
parameters. It returns the raw Slack response (`ok`, `messages`, `has_more`, and
`response_metadata.next_cursor`). Connector adapters may instead return their
next cursor in `next_arguments.cursor`. The callback runs on the caller's event
loop; it supplies credentials and enforces the application's access policy.
Authorization exceptions propagate. A provider error returned as `ok: false`
produces explicit context coverage; a failed later page preserves earlier messages.

The normalized event carries its current preview data bound to the event origin
and author. Passing that event to `thread_context` preserves its previews if the
provider read fails and merges them with fetched previews without duplication.
Pass `request_page=None` (or omit it) when application policy forbids a history
read, such as a direct-message workflow. This performs no I/O and reports
`history_not_requested` with partial or unavailable coverage. A reference copied
from a different event is rejected before the callback runs.

Context includes the parent and recent messages, with author IDs, file references,
and link previews. Current-message text, files, and composer `rich_text` blocks are
excluded because the current request already contains them; current link previews
remain. Source data is returned as bounded JSON text and must be presented as
untrusted reference material, separately from the user's request. Context reading
does not fetch linked URLs or download attachments.

Defaults limit the read to five pages, 40,000 content characters, 8,000 characters
per message, and eight seconds. Results report `complete`, `partial`, or
`unavailable`; missing roots, pagination limits, truncation, and provider failures
are explicit. The root is retained when older replies must be removed to meet the
budget. The timestamp cutoff is checked locally even when a connector cannot send
Slack's `latest` parameter. Code and quoted mentions remain literal text and do
not address the bot. Malformed supported events raise `ValueError`; ignored
subtypes and configured exclusions return `None`.

Existing `Slack.thread_history` remains supported and shares record normalization
with this API. Applications should persist admitted context if retries must see
the same snapshot.
