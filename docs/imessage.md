# iMessage through BlueBubbles

Run AgentChat anywhere that can reach your BlueBubbles server. BlueBubbles itself
must run on a Mac signed into Messages. Replies are sent as that Mac's Apple
account, so use a dedicated account/Mac for an agent that should receive every
incoming conversation. A separate bot identity is not created.

## Get the first reply

1. Follow the [BlueBubbles server setup](https://docs.bluebubbles.app/server/installation-guides)
   on the Mac. Grant its required Full Disk Access and Messages automation
   permissions, and confirm the Mac can send and receive iMessages manually.
   Set the BlueBubbles server password and enable its HTTP API/socket service.
   This adapter's basic text replies use AppleScript and do not require the
   BlueBubbles Private API or disabling System Integrity Protection.
2. Use the server's root URL, such as `http://127.0.0.1:1234` when AgentChat runs
   on the same Mac. Use HTTPS for a remote server. A reverse proxy must forward
   `/api/v1/` and `/socket.io/`, including WebSocket upgrades. No incoming public
   webhook or Firebase configuration is needed by this adapter.
3. From this repository:

   ```bash
   python -m pip install -e '.[imessage]'
   export BLUEBUBBLES_URL='http://127.0.0.1:1234'
   export BLUEBUBBLES_PASSWORD='your-server-password'
   export IMESSAGE_INSTANCE_ID='agent-mac'
   python examples/imessage_echo.py
   ```

   The stable instance ID keeps message/history identities unchanged if you move
   the server URL. If omitted, it is derived from the URL.
4. From a different Apple account, send `hello` to the Mac's iMessage account.
   Expect `hello` back. Sending from the same Apple account is ignored as an
   outgoing/self message. Stop with Ctrl+C.

For a test account that should only handle one conversation, restrict it before
starting: `export IMESSAGE_ALLOWED_CHATS='iMessage;-;+15551234567'`. Obtain the
actual chat GUID from BlueBubbles' chat API; do not infer it from a phone number.
The GUID above is an example. Comma-separate multiple GUIDs. Unset this variable
to receive all iMessage chats visible to the account. Programmatic
`allowed_chat_guids=[]` disables all conversations.

## Routing and server behavior

`IMessage(server_url=..., password=..., instance_id=...)` validates REST access,
then connects to authenticated Socket.IO `new-message` events. One chat GUID is
one conversation, including group chats; each message preserves its human sender.
Only iMessage chats are handled. SMS, outgoing messages, tapbacks/reactions,
service messages, and group-management events are ignored.

Replies post to the same existing chat using `/api/v1/message/text`. The returned
message GUID is the receipt; `tempGuid` is only a correlation value, not an
idempotency guarantee. Native inline reply threads are treated as part of the
chat, and outgoing replies appear in its main timeline. There is no automatic
history backfill or replay of events missed while offline. Socket reconnection
recovers future traffic, not missed messages.

Attachment-only messages retain file references, but downloads, uploads, and
rich replies are not implemented. See [shared capabilities](providers.md).

## Typing indicator

`set_status()` and `context.working()` show "typing…" in the chat; iMessage shows
no status text. This needs the BlueBubbles Private API with its helper connected.
Without it the server refuses the request and `set_status()` returns `False`, so
`working()` does nothing and the reply still goes out. Basic replies do not need
the Private API.

AgentChat sends `POST /api/v1/chat/{chat GUID}/typing` and repeats it every 5
seconds (`TYPING_INTERVAL` in `agentchat.channels.imessage.adapter`) because the
server stops typing after about 10 seconds. It stops repeating when the status
is cleared, `close()` runs, or 120 seconds (`TYPING_EXPIRY`) pass. Clearing sends
the Socket.IO `stopped-typing` event; the reply also ends the indicator. A custom
`client` needs `start_typing(chat_guid) -> bool` for typing; without it
`set_status()` returns `False`.

Streamed answers arrive as one reply when finished, and `ask()` raises
`TypeError` because iMessage has no buttons; see [Live controls](live-controls.md).

BlueBubbles authenticates using a password in request query parameters. The
adapter disables redirects, suppresses credential-bearing Socket.IO diagnostics,
and sanitizes REST errors. Use TLS remotely and exclude query strings from your
proxy access logs. Updating the server password requires updating the environment
and restarting the adapter.

## App features

Register handlers for BlueBubbles socket events (such as `updated-message`) and `/commands`. `IMessage(stop_keyword="stop")` lets a message that is exactly the keyword stop the sender's running turn. See [App features](app-features.md).

## Troubleshooting

| Symptom | Check |
| --- | --- |
| REST HTTP 401 | BlueBubbles server password matches `BLUEBUBBLES_PASSWORD`. |
| REST succeeds but the socket disconnects | `/socket.io/` is forwarded, the password is current, and the server is running. |
| Incoming messages appear but sends fail | Messages is signed in; BlueBubbles can automate Messages and send through AppleScript. |
| No inbound event | Test from another Apple account; check the chat allowlist and that the Mac actually received the message. |
| Works only while the Mac is awake | Keep the BlueBubbles host running and reachable. |
| `set_status()` returns `False` | Enable the BlueBubbles Private API and check that its helper is connected. |

Local tests run real Socket.IO and REST round trips against a simulated server.
They do not exercise macOS Messages. The live echo above is the final check.

Reference: [BlueBubbles REST API and events](https://docs.bluebubbles.app/server/developer-guides/rest-api-and-webhooks).
