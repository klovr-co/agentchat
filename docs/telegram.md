# Telegram

Connect a Telegram bot through the Bot API. Long polling is the default, so the
first check needs no public URL, tunnel, or model API key. Webhook mode is
available for deployments that already serve HTTPS.

## Get the first reply

1. In Telegram, open [@BotFather](https://t.me/BotFather), send `/newbot`, and
   choose a display name and a username ending in `bot`. BotFather replies with a
   token of the form `123456789:AA...`. Store it as `TELEGRAM_BOT_TOKEN`; the part
   before the colon is the bot's numeric ID.
2. From this repository, install and run:

   ```bash
   python -m pip install -e '.[telegram]'
   export TELEGRAM_BOT_TOKEN='123456789:your-bot-token'
   python examples/telegram_echo.py
   ```

   On start the adapter checks the token with `getMe`, removes any webhook
   (`deleteWebhook` without dropping pending updates), and long-polls `getUpdates`.
3. Open a private chat with your bot, press **Start**, and send `hello`. Expect
   `hello` back as a reply to your message. Stop with Ctrl+C.

The example echoes each accepted input. Replace its handler with your runner
after authorizing the sender. See [shared usage](providers.md) to run several
adapters or bind an existing runner directly.

### Groups and privacy mode

Add the bot to a group, then address it with `@yourbot question`, a command such
as `/status@yourbot`, or a reply to one of its messages. Bots have privacy mode
enabled by default, so Telegram only delivers those addressed messages to them
anyway. The adapter applies the same rule itself: unaddressed group messages never
reach the agent, even if you disable privacy mode with BotFather's `/setprivacy`
or make the bot a group admin. Privacy mode changes apply to groups the bot joins
afterwards; remove and re-add the bot to an existing group.

## Webhook mode

Set both variables to switch from polling to a webhook:

```bash
export TELEGRAM_WEBHOOK_URL='https://YOUR_HOST/webhooks/telegram'
export TELEGRAM_WEBHOOK_SECRET="$(python -c 'import secrets; print(secrets.token_urlsafe(32))')"
python examples/telegram_echo.py
```

The URL must use HTTPS and end in `/webhooks/telegram`. The secret may use
`A-Z`, `a-z`, `0-9`, `_` and `-`, up to 256 characters. The adapter listens on
`127.0.0.1:3980`, then calls `setWebhook` with the URL and secret. Forward a
public HTTPS tunnel/proxy to this port. Containers can set `TELEGRAM_HOST=0.0.0.0`;
`TELEGRAM_PORT` changes the port. Telegram sends each update with an
`X-Telegram-Bot-Api-Secret-Token` header; requests without the matching secret
get 401.

`close()` leaves the webhook set, so Telegram keeps queueing updates while the bot
is down and redelivers them after a restart. Telegram allows one consumer per
token: starting a poller with the same token removes the webhook.

For an existing aiohttp server, construct `Telegram(bot_token=..., webhook_url=...,
webhook_secret=...)`, host `telegram.create_app()` yourself, and bind the receiver
before serving requests. Register the webhook yourself, for example:

```bash
curl -s "https://api.telegram.org/bot$TELEGRAM_BOT_TOKEN/setWebhook" \
  -d "url=$TELEGRAM_WEBHOOK_URL" -d "secret_token=$TELEGRAM_WEBHOOK_SECRET" \
  -d 'allowed_updates=["message","callback_query"]'
```

The handler looks up the bot with `getMe` on the first update and returns 503
until that succeeds. Call `await telegram.close()` at shutdown; do not also call
`telegram.run()`.

## Routing and limits

- Each private chat and group is a conversation. In a forum supergroup, each topic
  is its own conversation and sets `message.thread_id` to the topic ID. Message
  IDs have the form `telegram:<bot_id>:<chat_id>:<message_id>`; button taps use
  `telegram:<bot_id>:callback:<callback_query_id>`.
- Messages from bots, channel posts, edits, service messages, and messages without
  text, a caption, or a file are ignored. The bot's `@username` is removed from the
  text, and from a command suffix (`/status@yourbot` becomes `/status`).
- Photos (largest size), documents, audio, video, voice notes, video notes,
  animations, and stickers become attachment references; captions become the
  message text. `download_attachment()` fetches the bytes on demand. The Bot API
  serves files up to 20 MB.
- Replies quote the original message, post in the same topic, and disable link
  previews. If the original was deleted, the reply still posts.
- Agent Markdown is converted to Telegram message entities, never `parse_mode`:
  bold (`**`), italic (`*` or `_`), strikethrough (`~~`), spoiler (`||`), inline
  code, fenced code blocks with a language, `http(s)` and `tg` links, headings
  (bold), bullet and numbered lists, and blockquotes. `__` and intra-word `_`/`*`
  stay literal, so `snake_case` and `__init__.py` are safe.
- A reply must fit 4,096 UTF-16 code units after conversion. Oversized replies
  raise `ValueError` before sending. Split longer output into separate
  `context.reply()` calls.

Replies, uploads, and status updates make one attempt; errors propagate. A Bot API
error raises `TelegramError` (a `RuntimeError`) with `error_code`, `description`,
and, on HTTP 429, `retry_after` seconds for your host's rate-limit scheduling.
Polling itself retries network errors, HTTP 5xx, and 429 with backoff; other
errors, such as 401 or 409, stop `run()`.

Telegram implements the optional capabilities:

- `set_status()` shows the typing indicator. Telegram clears it after about five
  seconds and has no custom text, so the adapter re-sends it every four seconds
  until you clear it with `""`, two minutes pass without another call, or the
  adapter closes. `context.working()` refreshes it for the duration of a handler.
- `reply_rich()` sends `RichReply.text` with one native Telegram reply markup in
  `blocks`, such as an inline keyboard:
  `RichReply(text="Continue?", blocks=({"inline_keyboard": [[{"text": "Yes", "callback_data": "yes"}]]},))`.
  Attachments are not supported.
- Inline keyboard taps arrive as ordinary messages whose text is the tapped
  button's label; `metadata["callback_data"]` holds its data. The adapter answers
  the tap so the button's spinner stops.
- `context.stream()` shows the answer while it is written. In private chats it
  uses Telegram's native draft (`sendMessageDraft`), then sends the finished
  answer as one message that replaces the draft. A draft is a preview that
  disappears about 30 seconds after its last update, so a long pause without new
  text hides it until the next update. In groups, or if Telegram refuses the
  draft, the first part is sent and later parts edit it in place. Updates go out
  at most every two seconds.
- `ask()` shows prompt buttons as an inline keyboard; see
  [Live controls](live-controls.md).
- `upload_files()` sends each file as a document to the source chat or topic, with
  `title` as the caption. The Bot API accepts uploads up to 50 MB. If a later file
  fails, earlier files were already delivered.

## Troubleshooting

Store the token as a secret. Revoke and regenerate it with BotFather's `/revoke`
if it leaks, then restart.

| Symptom | Check |
| --- | --- |
| `run()` fails with authentication failed | Copy the full token from BotFather, including the numeric prefix. |
| `run()` fails with HTTP 409 Conflict | Another process polls with the same token, or something set a webhook for it. Stop the other consumer. |
| The bot ignores group messages | Mention it, use `/command@yourbot`, or reply to its message. |
| A webhook never receives updates | Check the HTTPS URL and tunnel, and run `getWebhookInfo` for Telegram's last delivery error. |
| Webhook POST returns 401 | `TELEGRAM_WEBHOOK_SECRET` must match the `secret_token` registered with `setWebhook`. |
| HTTP 503 from this receiver | The bot identity is not yet known or the pending-work limit is full; Telegram retries. |
| Replies fail with HTTP 403 | The user blocked the bot, or the bot was removed from the group. |
| HTTP 429 | Pause sends for `TelegramError.retry_after` seconds in your host. |

Local tests exercise polling, webhooks, and sends against a simulated Bot API.
A live echo with your own bot remains the final deployment check.

Reference: [Telegram Bot API](https://core.telegram.org/bots/api).
