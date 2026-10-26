# Run messaging adapters

Choose the setup guide for the account you want to connect. Each guide ends with
a runnable echo example; no agent framework or model key is needed for that check.

| Provider | Install from checkout | Connection | Setup |
| --- | --- | --- | --- |
| Slack | `pip install -e .` | Socket Mode | [Slack](slack.md) |
| Discord | `pip install -e '.[discord]'` | Outbound gateway | [Discord](discord.md) |
| Teams | `pip install -e '.[teams]'` | Authenticated HTTPS endpoint | [Teams](teams.md) |
| iMessage | `pip install -e '.[imessage]'` | BlueBubbles Socket.IO + REST | [iMessage](imessage.md) |
| WhatsApp | `pip install -e '.[whatsapp]'` | Signed HTTPS webhook + Cloud API | [WhatsApp](whatsapp.md) |
| YCloud (WhatsApp) | `pip install -e '.[ycloud]'` | Signed HTTPS webhook + YCloud API | [YCloud](ycloud.md) |
| Telegram | `pip install -e '.[telegram]'` | Bot API long polling or HTTPS webhook | [Telegram](telegram.md) |

Use each provider's own package for imports, following the current Slack guide.
The `whatsapp`, `ycloud`, and `telegram` extras need
no additional dependency because aiohttp is already installed. Provider packages
and agent-framework integrations load only when imported. Python 3.11+ is required.

## Connect your runner

After completing the provider setups, construct just the channels you need:

```python
import asyncio

from agentchat import AgentChat
from agentchat.channels.discord import Discord
from agentchat.channels.imessage import IMessage
from agentchat.channels.teams import Teams
from agentchat.channels.whatsapp import WhatsApp


async def main():
    app = AgentChat(channels=[
        Discord.from_env(), Teams.from_env(), IMessage.from_env(), WhatsApp.from_env(),
    ])

    @app.on_message
    async def respond(context):
        # Authorize context.sender before fetching context or invoking agent tools.
        return context.message.text or "Received your attachment."

    try:
        await app.run()
    finally:
        await app.close()


asyncio.run(main())
```

Install several extras together, for example `pip install -e '.[discord,teams,imessage,whatsapp]'`.
The namespaces in `Message.id` and `conversation_id` keep provider/account histories
separate. Replacing the echo handler with an agent does not change transport setup.

If your host already owns deduplication, ordering, and history, bind it directly:

```python
channel = Discord.from_env()


async def receive(channel, message):
    # Authorize the sender, claim the ID, and serialize this conversation in your host.
    await channel.reply(message, "Received.")


async def main():
    channel.bind(receive)
    try:
        await channel.run()
    finally:
        await channel.close()
```

Call `asyncio.run(main())` in your executable. Retain the full source `Message`
when handing work to a worker: its metadata contains provider-specific reply routing.
Resolve that source from your trusted store rather than accepting destinations
or account IDs from an agent or browser input.

## Capabilities

These four new adapters implement the minimal `Channel` contract: receive,
lifecycle, and confirmed text replies. They normalize sender identity and
incoming attachment references. They do not download those attachments.

Telegram also implements `set_status()` (the typing indicator), `reply_rich()`
(native reply markup such as inline keyboards), `upload_files()`, and
`download_attachment()`. It does not implement native history or mirroring.

Discord, Teams, Telegram, WhatsApp, and YCloud also support streamed answers,
prompt buttons, and Stop/Queue/Steer; see [Live controls](live-controls.md).

WhatsApp and YCloud implement `reply_rich()` for one interactive message (buttons,
a list, a CTA URL or a Flow). Every provider takes handlers for its other events,
presses, forms and commands; private notices, deleting messages, home messages and
task progress depend on the provider, with fallbacks where it has none. Every
channel can ask a form in the chat with `open_form()`. See
[App features](app-features.md).

Discord, Teams, WhatsApp, YCloud, and iMessage implement `set_status()` as their
typing indicator. iMessage needs the BlueBubbles Private API and returns `False`
without it.
These providers show no status text and clear the indicator after a few
seconds, so the adapter sends it again until `set_status(source, "")`, the
reply, `close()`, or `TYPING_EXPIRY` (120 seconds) ends it. `context.working()`
uses this for the whole handler.

History uses the application's local `State`; default `MemoryState` is lost on
restart. Native history, working indicators, rich replies, file transfers, and
cross-surface mirroring remain optional capabilities. Unsupported
rich/file/mirror operations raise `TypeError`. Slack retains its existing capabilities. Applications choose text
or link fallbacks explicitly rather than passing Slack Block Kit to another provider.

Long responses raise a validation error at the provider's documented limit;
there is no silent truncation or automatic multi-message send. If you split an
answer, track each `context.reply()` independently because earlier chunks may
have succeeded before a later send fails.

## Delivery and shutdown

Each adapter admits at most `max_pending_events=64` in-process handler tasks.
`AgentChat` deduplicates accepted message IDs and serializes handlers by
conversation using the configured `State`.

- Teams returns 202 after admission; WhatsApp and YCloud return 200. A full HTTP receiver
  returns 503 before admission so the provider can apply its retry policy.
  WhatsApp batches are admitted atomically.
- Telegram webhooks return 503 the same way. Telegram polling stops reading at
  the first update it cannot admit and polls again from it, so Telegram redelivers
  it instead of the adapter dropping it.
- Discord and BlueBubbles callbacks cannot ask the gateway to replay an event.
  At capacity, the adapter logs a warning and drops the excess event. Reconnects
  do not provide durable catch-up. Size the limit/host for the expected workload.
- `close()` stops admission, drains handlers for up to `shutdown_timeout=20`
  seconds, then cancels unfinished handlers and releases clients/listeners.
  `run()` cleans up on cancellation or connection/startup failure. Instances are
  intended for a single run; create a new instance after closing one.

Accepted work is not a durable queue. A crash after webhook acknowledgement can
lose a pending handler; a failed handler is logged and not automatically replayed.
Use application-owned durable ingestion/state/outboxes for stronger guarantees.
Persisting only a claim before work is durable can suppress a provider retry.

Replies make a single send attempt and return a normalized assistant message only
after obtaining a provider receipt. The core appends that confirmed result to
history. A receipt means provider acceptance, not that a human received/read it.
Timeouts may occur after a provider accepts a message; avoid blindly retrying
uncertain writes. Rate-limit scheduling and delivery reconciliation belong to the
host. Injected clients must preserve these receipt/retry semantics.

## Validate a deployment

The test suite covers provider event fixtures, Microsoft/Discord SDK behavior,
authentication rejection, local HTTP and Socket.IO round trips, import isolation,
receipts, failure handling, and cleanup. It uses no live platform credentials.
Run the guide's live echo check for each configured account before enabling your
agent, including a channel/thread check where relevant. Successful local tests
do not verify provider installation, tenant permissions, phone registration, or
macOS automation permissions.
