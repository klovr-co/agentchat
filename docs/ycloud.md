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
   `https://YOUR_HOST/webhooks/ycloud` and subscribe it to
   `whatsapp.inbound_message.received`. Copy the endpoint's signing secret into
   `YCLOUD_WEBHOOK_SECRET` exactly as shown, including any `whsec_` prefix, and
   restart the example.
5. From your own WhatsApp account, send `hello` to the business number. Expect
   `hello` back. Stop with Ctrl+C.

## Routing and limits

Each `(business phone number, sender phone number)` pair is a conversation.
`Message.id` uses the WhatsApp `wamid`, so YCloud retries of the same inbound
message are deduplicated. Events for other business numbers, status updates
(`whatsapp.message.updated`), group messages, reactions, system events, and
unsupported message types do not invoke the agent. Text, button/list reply titles,
and image/audio/video/document/sticker references are normalized; an attachment's
`id` is the WhatsApp media ID, or YCloud's download `link` when no ID is present.
The customer profile name becomes `sender.display_name`, and `metadata` carries
`ycloud_message_id` and `waba_id` alongside the `wamid`.

`YCloud(phone_number=..., api_key=..., webhook_secret=...)` checks the
`YCloud-Signature` header (`t=<unix seconds>,s=<hex HMAC-SHA256>` over
`"{t}.{raw body}"`) before parsing, and rejects timestamps more than five minutes
from the receiver's clock. The reverse proxy must preserve the request body and
signature header exactly.

Replies are sent with `POST /v2/whatsapp/messages/sendDirectly`, quote the original
message through `context.message_id`, disable link previews, and require nonempty
text of at most 4,096 characters. HTTP errors, rate limits, and timeouts propagate
without retries. A returned message confirms that YCloud accepted it, not handset
delivery or read status.

Free-form replies are subject to WhatsApp's customer-service window, generally 24
hours from the user's last message. Template messages, proactive sends, group
conversations, media downloads/uploads, and delivery-status callbacks are outside
this adapter's current interface.

For an existing aiohttp server, host `ycloud.create_app()` yourself and bind the
receiver before serving requests. Call `await ycloud.close()` at shutdown; do not
also call `ycloud.run()`. See [shared usage and delivery semantics](providers.md).

## Troubleshooting

| Symptom | Check |
| --- | --- |
| POST returns 401 | Correct endpoint secret, unmodified raw body/signature header, and the server clock. |
| Webhook succeeds but nothing happens | Subscription to `whatsapp.inbound_message.received` and `YCLOUD_PHONE_NUMBER` matching the receiving number in E.164 form. |
| Send returns HTTP 400/401/403 | API key, sender number registration, and an open customer-service window. |
| HTTP 503 from this receiver | Its pending-work limit is full; see the shared delivery guide. |

Local tests exercise signed webhooks and actual HTTP sends to a simulated YCloud
endpoint. They do not establish that your YCloud account is provisioned correctly.
Verify the live echo before connecting an agent.

Reference: [YCloud API documentation](https://docs.ycloud.com/reference/introduction).
