# Microsoft Teams

Connect a single-tenant Teams bot in Microsoft's public cloud using the
Microsoft 365 Agents SDK. You need a tenant that permits installing your Teams
app, an Azure Bot registration, and a public HTTPS endpoint. A tenant administrator
may need to approve custom app upload or installation.

## Get the first reply

1. Create a **single-tenant** Microsoft Entra application and a client secret.
   Record its **Application (client) ID**, **Directory (tenant) ID**, and the
   secret **value** (not the secret ID). Create an Azure Bot resource using this
   app ID and tenant; enable its **Microsoft Teams** channel. Microsoft's
   [Agents SDK quickstart](https://learn.microsoft.com/en-us/microsoft-365/agents-sdk/quickstart)
   describes the registration options.
2. From this repository, install and start the echo server:

   ```bash
   python -m pip install -e '.[teams]'
   export TEAMS_APP_ID='application-client-id'
   export TEAMS_TENANT_ID='directory-tenant-id'
   export TEAMS_APP_PASSWORD='client-secret-value'
   python examples/teams_echo.py
   ```

   It listens on `127.0.0.1:3978`. Forward a public HTTPS URL through a development
   tunnel to this port, or deploy behind an HTTPS reverse proxy. For containers,
   set `TEAMS_HOST=0.0.0.0`; `TEAMS_PORT` overrides the port.
3. Set the Azure Bot **Messaging endpoint** to
   `https://YOUR_HOST/api/messages`. Keep bearer authorization enabled. Anonymous
   curl requests are expected to return 401; they do not test a Teams conversation.
4. In the [Teams Developer Portal](https://dev.teams.microsoft.com/), create a
   Teams app and add a bot capability using the same app ID. Enable **Personal**
   scope first; optionally add **Team** and **Group Chat** scopes. Complete the
   portal's required app metadata/icons, download the app package, and upload it
   through Teams **Apps → Manage your apps → Upload an app** (subject to tenant
   policy). Install it for yourself or in your test team.
5. Send `hello` in the bot's personal chat. Expect `hello` back. In a team/channel
   or group chat, send `@YourBot hello`. Stop the local server with Ctrl+C.

## Routing and hosting

Personal chats accept ordinary messages. Group chats and channels require a bot
mention on each incoming message. Personal and group chats retain their shared
conversation; channel replies preserve the original root thread. Only the bot's
own mention is removed. Sender IDs remain Teams IDs scoped by the configured
tenant; display names do not authorize access.

`Teams(app_id=..., app_password=..., tenant_id=...)` validates Microsoft JWTs,
audience/issuer, the configured tenant, and HTTPS Microsoft service URLs. This
adapter supports public-cloud single-tenant client-secret authentication. It does
not add Graph chat-reading permissions, tenant-wide message subscriptions,
anonymous emulator access, or sovereign-cloud configuration.

For an existing aiohttp host, use `teams.create_app()` and manage its runner
yourself. This keeps the authentication middleware and `/api/messages` route.
Bind a receiver before serving requests and call `await teams.close()` at
shutdown. Use either your host or `teams.run()`, not both. Supplying `adapter=`
injects a Microsoft `CloudAdapter` for tests or a host-owned connector factory;
the adapter's turn-error handler is replaced so errors propagate rather than
posting diagnostics to the conversation.

Normal messages are acknowledged with HTTP 202 after admission to bounded
background work; replies use a fresh continuation context, not an expired HTTP
turn. A full queue returns 503 before admission. See [delivery semantics](providers.md#delivery-and-shutdown).

Replies use plain text and accept up to 24,000 UTF-8 bytes, leaving room for the
activity envelope below Teams' message limit. Split larger answers into separate
replies. Incoming file/card metadata is retained, but download, upload, card
actions, rich replies, and native-history capabilities are not implemented here.

## Typing indicator

`set_status(source, "thinking")` sends a `typing` activity to the source
conversation and repeats it every 3 seconds, because Teams clears it quickly.
`set_status(source, "")` stops it; Teams shows no status text.

## App features

Register handlers for other activities (such as `installationUpdate`), card actions, dialogs and `/commands`; an invoke handler's return value is the invoke response. Teams also sends targeted (private) messages, deletes messages, shows task progress, and stops the turn when someone presses Stop on a streamed answer. See [App features](app-features.md).

## Troubleshooting and maintenance

| Symptom | Check |
| --- | --- |
| HTTP 401 from real Teams traffic | Azure Bot app ID, tenant, client-secret value/expiry, and proxy forwarding of `Authorization`. |
| Bot not available in Teams | Teams channel enabled in Azure Bot; matching manifest bot ID; tenant custom-app policy. |
| DM works but channel does not | Install the app in that team and mention it on each message. |
| No traffic at `/api/messages` | Public HTTPS endpoint and tunnel/proxy path point to this process. |
| Send fails after setup worked | Rotate an expired client secret and restart; verify the app remains installed. |

Local tests use Microsoft's real activity, continuation, and routing code with
fake connectors, plus unauthenticated-request rejection. They do not validate
your Azure registration. The first live Teams reply above verifies that setup.
