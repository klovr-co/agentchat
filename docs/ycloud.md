# YCloud (WhatsApp)

Connect a WhatsApp Business number that is registered through
[YCloud](https://www.ycloud.com/), a WhatsApp Business Solution Provider. Use this
adapter instead of [`WhatsApp`](whatsapp.md) when your number is managed in the
YCloud console rather than directly in Meta's Cloud API. Messages arrive through
YCloud's signed webhooks and replies go through YCloud's send API.

## Get the first reply

1. In the [YCloud console](https://www.ycloud.com/console/), connect your WhatsApp
   Business Account and register a business phone number. Record the number in
   E.164 form, for example `+15550001111`.
2. Under **Developers → API Keys**, create or copy an API key.
3. From this repository, install and start the server:

   ```bash
   python -m pip install -e '.[ycloud]'
   export YCLOUD_PHONE_NUMBER='+15550001111'
   export YCLOUD_API_KEY='your-api-key'
   export YCLOUD_WEBHOOK_SECRET='whsec_...'   # from step 4
   python examples/ycloud_echo.py
   ```

   It listens on `127.0.0.1:3981`. Forward a public HTTPS tunnel/proxy to this port.
   Containers can set `YCLOUD_HOST=0.0.0.0`; `YCLOUD_PORT` changes the port.
4. Under **Developers → Webhooks**, add an endpoint with the URL
   `https://YOUR_HOST/webhooks/ycloud` and subscribe it to the
   [events below](#webhook-events) you need; the echo needs only
   `whatsapp.inbound_message.received`. Copy the endpoint's signing secret into
   `YCLOUD_WEBHOOK_SECRET` exactly as shown, including any `whsec_` prefix, and
   restart the example.
5. From your own WhatsApp account, send `hello` to the business number. Expect
   `hello` back. Stop with Ctrl+C.

## Webhook events

Every POST is checked before its body is parsed: the `YCloud-Signature` header
(`t=<unix seconds>,s=<hex HMAC-SHA256>` over `"{t}.{raw body}"`) must match, and
the timestamp must be within five minutes of the receiver's clock. Unsigned or
stale requests get 401. The reverse proxy must preserve the body and header exactly.

| Event `type` | Delivered to | Notes |
| --- | --- | --- |
| `whatsapp.inbound_message.received` | Agent handler (`bind` / `AgentChat`) as `Message(role="user")` | Text, button/list replies, image/audio/video/document/sticker. Groups, reactions and other numbers are ignored. |
| `whatsapp.message.updated` | Event handler as `YCloudStatus` | Delivery status of messages you sent. Never an agent turn. |
| `whatsapp.smb.message.echoes` | Event handler as `Message(role="assistant")` | Only with `YCloud(..., smb_echoes=True)` (or `YCLOUD_SMB_ECHOES=1`); ignored otherwise. Never an agent turn. |
| Anything else | Nobody | Acknowledged with 200 and dropped, including `whatsapp.smb.message.created`. |

Each `(business phone number, customer phone number)` pair is a conversation,
`ycloud:{business}:{customer}`. `Message.id` uses the WhatsApp `wamid`, so YCloud
retries of the same message are deduplicated. The customer profile name becomes
`sender.display_name`, and `metadata` carries `ycloud_message_id`, `waba_id`, and
(for media) a `media` mapping used by [`download_media`](#media-download).

### The event handler

```python
from agentchat import Message
from agentchat.channels.ycloud import YCloud, YCloudStatus

ycloud = YCloud.from_env()

async def on_event(channel: YCloud, event: YCloudStatus | Message) -> None:
    if isinstance(event, YCloudStatus):
        await outbox.record_status(event)   # your durable store
    else:                                   # Business App echo, role "assistant"
        await inbox.record_staff_reply(event)

ycloud.bind_events(on_event)
```

The handler is awaited inside the webhook request, and the 200 is sent only after
it returns. If it raises, the receiver returns 500 and YCloud redelivers the event
(by default after 10 s, 30 s and 5 min). Only the exception type is logged. Keep the
handler short, for example a single database insert: YCloud asks for a response in
under 6 seconds. Deduplicate by `event_id` (statuses) or `Message.id` (echoes),
because redelivery repeats events. Without a bound handler these events are
acknowledged and dropped.

### Delivery status

`YCloudStatus` carries YCloud's raw values: `status` (`sent`, `delivered`, `read`,
`failed`; any other value YCloud sends is passed through), `message_id` (YCloud
`id`), `wamid`, `external_id`, `recipient`, `conversation_id`, `error_code`,
`error_message`, and `whatsapp_api_error` (`code`, `message`, `type`, `error_data`).
`error_kind` classifies failures with the same values as
[`YCloudError.kind`](#errors). `timestamp` is the status's own time (`sendTime`,
`deliverTime`, `readTime`), else `updateTime`, else the event's `createTime`.

**Status events are not ordered.** YCloud does not guarantee their order:
`delivered` can arrive after `failed`, `read` can arrive without `delivered`, and
`delivered` can repeat when the customer uses several devices. Apply updates
forward only, for example `accepted < sent < delivered < read`, with `failed`
recorded but never overwriting a later delivered or read state. Never let an
older update replace a newer one.

### Business App echoes (coexistence)

With [coexistence](https://docs.ycloud.com/en/documentation/whatsapp-business-platform/accounts-and-business-identity/whatsapp-business-app-coexistence.md),
staff can also reply from the WhatsApp Business app on the same number. With
`smb_echoes=True`, those replies arrive as `Message(role="assistant")` in the
customer's conversation ID, with `metadata["echo"] = True` and `metadata["to"]`
set to the customer. They go to the event handler only, so they never trigger an
AI turn. `reply()` refuses an echo as its source; reply to the customer's message.

## Sending

All sends use `POST /v2/whatsapp/messages/sendDirectly` and make exactly one
attempt. A timeout or connection failure raises `YCloudError(kind="uncertain")`:
the message may or may not have been sent, so do not resend blindly. A returned
message confirms that YCloud accepted it, not handset delivery or read status.

```python
reply = await ycloud.reply(source, "Your order is ready", external_id="outbox-42")
sent = await ycloud.send_template(
    "+15551234567", "order_ready", "en_US",
    [{"type": "body", "parameters": [{"type": "text", "text": "A-17"}]}],
    external_id="outbox-43",
)
```

- `reply(source, text, *, external_id=None)` quotes the source through
  `context.message_id`, disables link previews, and requires nonempty text of at
  most 4,096 characters. It needs an open customer-service window.
- `send_template(destination, name, language_code, components=None, *, external_id=None)`
  sends an approved template to an E.164 number. It needs no source message, so it
  works after the window has closed. `components` follows YCloud's template send
  schema. The receipt is a `Message(role="assistant")` with empty `text` and
  `metadata["template_name"]`.
- `external_id` is sent as YCloud's `externalId` and comes back on every
  `YCloudStatus`, so you can match status events to your own outbox rows.
  **`externalId` does not prevent duplicate sends.** It is a correlation label:
  YCloud's docs do not say that a repeated value is rejected, and this adapter
  does not check it. Make sends idempotent in your own store (claim the outbox
  row first) before calling `reply` or `send_template`.

`list_templates(status="APPROVED", language=None)` calls
`GET /v2/whatsapp/templates` with `filter.status`/`filter.language`, 100 per page,
until a short page (at most 100 pages). Pass `status=None` for every status, or a
comma-separated list such as `"APPROVED,PAUSED"`. Each `YCloudTemplate` has
`name`, `language`, `category`, `status`, `body` (the BODY component's text) and
the raw `components`.

### Errors

Failed calls raise `YCloudError`, a `RuntimeError`. Branch on `kind`, never on
the message text. The error also carries `status` (HTTP), `code` (YCloud
`error.code`), `whatsapp_code` (Meta `error.whatsappApiError.code`) and
`request_id`. Its text contains only these codes: never the message body, the API
key or the webhook secret.

| `kind` | Cause | What the app should do |
| --- | --- | --- |
| `window_expired` | Meta `131047`: more than 24 h since the customer's last message | Offer a template (`send_template`) instead of free text. |
| `template_parameters` | Meta `132000`: parameter count does not match the template | Fix the components. |
| `template_not_found` | Meta `132001`: template missing in that language or not approved | Check name, language and status (`list_templates`). |
| `template_unavailable` | YCloud `WHATSAPP_TEMPLATE_UNAVAILABLE` (403) | Same as above. |
| `balance_insufficient` | YCloud `BALANCE_INSUFFICIENT` (403) | Top up the YCloud balance. |
| `unauthorized` | HTTP 401 / `UNAUTHORIZED` | Check the API key. |
| `rate_limited` | HTTP 429 | Back off and respect `Retry-After`; this adapter does not retry. |
| `rejected` | Any other non-2xx response | Inspect `code` and `whatsapp_code`. |
| `uncertain` | Timeout, connection error, or unreadable response | The send may have happened; reconcile through status events. |
| `media_too_large`, `media_integrity`, `media_unavailable` | See [Media download](#media-download) | |

`131047` can also arrive later as a `failed` status with `error_kind ==
"window_expired"`. The same mapping applies to status events.

## Media download

```python
file = await ycloud.download_media(message)   # or message.metadata["media"]
file.content, file.media_type, file.name      # agentchat.models.DownloadedFile
```

Inbound media (and echo media) keep `link`, `id`, `sha256`, `mime_type` and
`filename` in `metadata["media"]`; the caption is the message text. Store that
mapping if you download later.

- The signed `link` works without credentials for a few minutes. The adapter tries
  it first, and only on HTTP 401/403 retries once with the `X-API-Key` header.
- Only links under `{API_ROOT}/whatsapp/media/` are fetched, which in production is
  `https://api.ycloud.com/v2/whatsapp/media/`. Other hosts, schemes, ports,
  credentials in the URL and other API paths raise `ValueError` before any request
  is made. Redirects are not followed.
- Downloads stop at `max_media_bytes` (default 25 MB, set on the constructor or
  per call with `max_bytes=`): `kind="media_too_large"`.
- When `sha256` is present, the content must match it (YCloud sends base64; hex is
  also accepted): otherwise `kind="media_integrity"`.
- Any other status, or an empty body, raises `kind="media_unavailable"` with
  `status` set.
- **Files expire after 30 days.** Download and store what you need to keep.

## Pricing note

Effective **1 October 2026**, WhatsApp service messages (free-form replies inside
the customer-service window) are charged per delivered message after the first
**1,000 free service messages per business phone number per month**. Utility
templates are charged per delivered message both inside and outside the window,
and reactions are free. YCloud's pages describe the charge after the allowance as
the *service* rate; they do not say it equals the Utility rate. Check
[YCloud pricing](https://www.ycloud.com/pricing) for the rates in your market.

## Hosting and troubleshooting

For an existing aiohttp server, host `ycloud.create_app()` yourself and bind the
receiver and event handler before serving requests. Call `await ycloud.close()` at
shutdown; do not also call `ycloud.run()`. See
[shared usage and delivery semantics](providers.md). Group conversations and media
uploads are outside this adapter's interface.

| Symptom | Check |
| --- | --- |
| POST returns 401 | Correct endpoint secret, unmodified raw body/signature header, and the server clock. |
| POST returns 500 | Your event handler raised; YCloud will redeliver. |
| Webhook succeeds but nothing happens | Event subscriptions, `bind_events`, `smb_echoes`, and `YCLOUD_PHONE_NUMBER` matching the number in E.164 form. |
| `YCloudError` on send | Its `kind`, `code` and `whatsapp_code` (see [Errors](#errors)). |
| HTTP 503 from this receiver | Its pending-work limit is full; see the shared delivery guide. |

## Verification status

The local tests exercise signed webhooks, sends, template paging and media
downloads against a simulated YCloud built from the documented shapes. They do not
prove that the live service behaves the same, or that your account is provisioned
correctly. These points come from YCloud's docs but have not been checked against
the live API:

- Status events may omit `from`. The adapter accepts them, but drops them when
  `from` names another number.
- No documented example shows a `failed` event's own timestamp. The adapter falls
  back to `updateTime`, then the event's `createTime`.
- The HTTP status that accompanies a synchronous `131047` is not documented; the
  mapping uses the codes only.
- Whether YCloud rejects a repeated `externalId` is not documented. Treat it as
  not deduplicating.
- Whether media links ever redirect is not documented. Redirects are refused, so a
  redirecting link would raise `media_unavailable`.

## Sources

Pages opened on 21 Oct 2026:

- [Configure webhooks](https://docs.ycloud.com/en/api-reference/guides/api-fundamentals/configure-webhooks.md) and [Implement a webhook receiver](https://docs.ycloud.com/en/api-reference/guides/api-fundamentals/implement-a-webhook-receiver.md): signature, 6-second response target, retry intervals.
- [Message status webhook examples](https://docs.ycloud.com/en/api-reference/guides/examples/webhook-examples/whatsapp-message-updated-webhook-examples.md): status fields and ordering.
- [Inbound message webhook examples](https://docs.ycloud.com/en/api-reference/guides/examples/webhook-examples/whatsapp-inbound-message-webhook-examples.md): media `link`, `sha256`, signed-link and 30-day expiry.
- [Business App sent message sync examples](https://docs.ycloud.com/en/api-reference/guides/examples/webhook-examples/whatsapp-business-app-sent-message-sync-webhook-examples.md): `whatsapp.smb.message.echoes`.
- [Handle errors](https://docs.ycloud.com/en/api-reference/guides/api-fundamentals/handle-errors.md) and [WhatsApp error code](https://docs.ycloud.com/en/api-reference/guides/whatsapp-platform/whatsapp-error-code.md): error body, `BALANCE_INSUFFICIENT`, `WHATSAPP_TEMPLATE_UNAVAILABLE`, 131047, 132000, 132001; `externalId` and duplicate POSTs.
- [List templates](https://docs.ycloud.com/api-reference/whatsapp-templates/list-templates.md): filters and paging.
- [WhatsApp pricing](https://docs.ycloud.com/en/documentation/whatsapp-business-platform/pricing-limits-and-quality/whatsapp-pricing.md) and [October 2026 pricing update](https://www.ycloud.com/blog/whatsapp-api-message-pricing-update-effective-october-1-2026): the 1,000 free service messages.
- [YCloud SDKs](https://github.com/YCloud-Developers) (`ycloud-sdk-php`, OpenAPI in `ycloud-sdk-java`): request and response field names.
