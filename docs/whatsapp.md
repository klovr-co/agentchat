# WhatsApp Business Cloud API

Connect an official WhatsApp Business phone number through Meta's Cloud API.
This adapter does not automate a personal WhatsApp account or require WhatsApp
Web/QR pairing. For an initial test, use the test phone number supplied by Meta.

## Get the first reply

1. In [Meta for Developers](https://developers.facebook.com/apps/), create/configure
   an app with the WhatsApp product/use case and connect a business portfolio.
   Open **WhatsApp → API Setup**, select the test number, and add your own WhatsApp
   account as a verified test recipient. Record the **Phone number ID** and the
   temporary access token. The ID is not the visible phone number or business ID.
2. Under the app's **Settings → Basic**, obtain the **App secret**. Choose a separate
   random webhook verification token, for example with
   `python -c 'import secrets; print(secrets.token_urlsafe(32))'`.
3. From this repository, install and start the server:

   ```bash
   python -m pip install -e '.[whatsapp]'
   export WHATSAPP_PHONE_NUMBER_ID='numeric-phone-number-id'
   export WHATSAPP_ACCESS_TOKEN='your-access-token'
   export WHATSAPP_APP_SECRET='your-meta-app-secret'
   export WHATSAPP_VERIFY_TOKEN='your-chosen-verification-token'
   python examples/whatsapp_echo.py
   ```

   It listens on `127.0.0.1:3979`. Forward a public HTTPS tunnel/proxy to this port.
   Containers can set `WHATSAPP_HOST=0.0.0.0`; `WHATSAPP_PORT` changes the port.
4. In WhatsApp's webhook configuration, enter
   `https://YOUR_HOST/webhooks/whatsapp` as the callback URL and the same
   verification token. Verify/save, then subscribe to the **messages** field
   for the connected WhatsApp Business Account. Keep the app subscribed to that
   account when moving from a test number to a production number.
5. From the verified test recipient, send `hello` to the test business number.
   Expect `hello` back. Sending a user message opens the customer-service window
   needed for a free-form text reply. Stop with Ctrl+C.

## Routing and limits

Each `(phone_number_id, sender WhatsApp ID)` pair is a conversation. Signed webhook
batches can contain multiple inputs; sender profile names are matched by WhatsApp
ID. Events for other phone-number IDs, status receipts, reactions, system events,
and unsupported message types do not invoke the agent. Text, button/list reply
titles, and image/audio/video/document/sticker references are normalized.

`WhatsApp(phone_number_id=..., access_token=..., app_secret=..., verify_token=...)`
checks the raw-body `X-Hub-Signature-256` HMAC before parsing messages. The webhook
verification token is for the GET challenge; the app secret authenticates POSTs.
The reverse proxy must preserve the request body and signature header exactly.

Replies quote the original message through `context.message_id`, disable link
previews, and require nonempty text of at most 4,096 characters. They use Graph
API `v23.0` by default; set `WHATSAPP_API_VERSION` to another supported version
when upgrading. HTTP errors, rate limits, and timeouts propagate without retries.
A returned `wamid` confirms API acceptance, not handset delivery/read status.

Free-form replies are subject to Meta's customer-service window, generally 24
hours from the user's last message. Template messages, proactive campaigns,
group conversations, media downloads/uploads, and delivery-status callbacks are
outside this adapter's current interface. Do not use a stored source message to
assume the text-reply window remains open.

For an existing aiohttp server, host `whatsapp.create_app()` yourself and bind the
receiver before serving requests. This exposes the same verification and signed
webhook handlers. Call `await whatsapp.close()` at shutdown; do not also call
`whatsapp.run()`. See [shared usage and delivery semantics](providers.md).

## Prompts

`context.ask()` sends the question as an interactive reply: up to three reply
buttons with titles of 20 characters, otherwise a list of up to ten rows.
`prompt_style="reactions"` asks for a reaction with a listed emoji instead. See
[Live controls](live-controls.md).

## Typing indicator

`set_status(source, "thinking")` shows "typing…" to the sender and repeats it
every 20 seconds, because WhatsApp clears it after 25 seconds or when the reply
arrives. WhatsApp ties the indicator to the incoming message, so it also marks
that message as read. `set_status(source, "")` stops refreshing; WhatsApp shows
no status text.

## Production setup and troubleshooting

Replace Meta's temporary token with a system-user token that has access to the
registered business phone number and `whatsapp_business_messaging` permission.
Business/account setup and subscription operations can also require
`whatsapp_business_management`. Complete Meta's required business/number
registration and app access steps before serving real users. Store tokens as
secrets, monitor expiry/revocation, and restart after rotation.

| Symptom | Check |
| --- | --- |
| Webhook verification fails | HTTPS URL/path, server is running, and matching `WHATSAPP_VERIFY_TOKEN`. |
| POST returns 401 | Correct Meta app secret and an unmodified raw body/signature header. |
| Webhook verifies but no messages arrive | Subscribe to `messages` on the correct business account and check the phone-number ID. |
| Send returns HTTP 400/401/403 | Token expiry/permissions, phone-number ID, test recipient verification, and an open customer-service window. |
| HTTP 503 from this receiver | Its pending-work limit is full; see the shared delivery guide. |

Local tests exercise signed webhooks and actual HTTP sends to a simulated Graph
endpoint. They do not establish that your Meta account is provisioned correctly.
Verify the live echo before connecting an agent.

Reference: [WhatsApp Cloud API documentation](https://developers.facebook.com/docs/whatsapp/cloud-api/).
