# Discord

Connect a Discord bot through its outbound gateway. No public webhook, model API
key, or privileged Message Content intent is required for DMs and explicit mentions.

## Get the first reply

1. In the [Discord Developer Portal](https://discord.com/developers/applications),
   create an application. Under **Bot**, create/reset its token and store it as
   `DISCORD_BOT_TOKEN`. Use the bot token, not the client secret.
2. Under **OAuth2 → URL Generator**, select the `bot` scope and the permissions
   **View Channels**, **Send Messages**, **Read Message History**, and **Send
   Messages in Threads**. Open the generated URL and install the bot in a test
   server you manage. Channel overrides must grant these permissions too.
3. From this repository, install and run:

   ```bash
   python -m pip install -e '.[discord]'
   export DISCORD_BOT_TOKEN='your-bot-token'
   python examples/discord_echo.py
   ```

4. Send `hello` to the bot in a DM, or `@YourBot hello` in the test channel. The
   bot should reply `hello` in the same destination. Stop with Ctrl+C.

The example echoes each accepted input. Replace its handler with your runner
after authorizing the sender. See [shared usage](providers.md) to run several
adapters or bind an existing runner directly.

## Routing and replies

- Each DM channel, guild channel, and native Discord thread has its own shared
  conversation. Native threads set `message.thread_id` to the thread channel ID.
  Use separate native threads when tasks need separate history.
- Guild messages require an explicit bot mention or a resolved reply to this bot.
  When replying, leave Discord's reply mention enabled so the default gateway
  intents receive the message content. Unmentioned thread messages are ignored;
  this adapter does not automatically subscribe to thread follow-ups.
- Messages from bots, webhooks, and system events are ignored. Only this bot's
  mention is removed from input text. Attachments become metadata references;
  their contents are not fetched.
- Replies use a Discord message reference in the original channel/thread.
  If the original message was deleted, the reply still posts there. Outbound
  `@everyone`, role/user mentions, and reply-author pings are disabled.
- A reply must fit 2,000 UTF-16 code units. Oversized replies raise `ValueError`
  before sending. Split longer output into separate `context.reply()` calls.

`Discord(bot_token=..., max_pending_events=64, shutdown_timeout=20)` also accepts
an owned `discord.Client` through `client=` and an owned `DiscordRESTClient`
through `send_client=`. The adapter installs the client's `on_message` callback
and closes both clients on shutdown. If supplying custom intents, retain guild,
guild-message, and DM-message events. Reading unmentioned guild content requires
enabling Message Content in both the portal and your custom client; this does not
change the adapter's mention filter.

The gateway reconnects through discord.py. Replies use a separate, single-attempt
HTTP request, including on HTTP 429 or 5xx; errors propagate. The application
owns rate-limit scheduling and recovery of uncertain sends.

## Typing indicator

`set_status(source, "thinking")` shows "Bot is typing…" in the source channel
and sends it again every 8 seconds, because Discord clears it after about ten.
`set_status(source, "")` stops it; Discord shows no status text. A custom
`send_client` without `trigger_typing(channel_id)` makes `set_status()` return
`False`. Each refresh is one `POST /channels/{id}/typing`, which counts toward
Discord's rate limits.

## App features

Register handlers for gateway events, component and modal interactions, and slash commands; their return value is the interaction response, which can be private or open a modal. Discord also deletes messages and shows task progress. See [App features](app-features.md).

## Troubleshooting and maintenance

| Symptom | Check |
| --- | --- |
| Login fails | Reset the bot token, update the environment, and restart. |
| Bot receives DMs but misses guild messages | Mention the bot and check channel permissions; the default does not read arbitrary channel content. |
| Replies fail with HTTP 403 | Grant Send Messages/Read Message History, plus Send Messages in Threads for a thread. |
| HTTP 429 | Reduce/pause outgoing sends and apply the platform's rate-limit policy in your host. |
| A thread gets no reply | The bot must have access; check archived/locked/private thread permissions. |

Local tests exercise real discord.py message parsing and a local HTTP send server.
A live bot-token round trip remains the final deployment check.

References: [discord.py gateway/intents](https://discordpy.readthedocs.io/en/stable/intents.html),
[Discord message API](https://docs.discord.com/developers/resources/message).
