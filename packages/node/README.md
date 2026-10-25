# @berriai/agentchat

An ESM Node.js SDK backed by AgentChat's shared Rust dispatch and native Slack
Socket Mode implementation. Requires Node.js 22 or newer. This alpha supports
Slack; other channels and custom JavaScript state implementations are not yet
part of the Node API.

```ts
import { AgentChat, Slack } from "@berriai/agentchat";

const app = new AgentChat({ channels: [Slack.fromEnv()] });
app.onMessage(async (context) => {
  console.log(context.conversation.id);
  return `You said: ${context.message.text}`;
});

process.once("SIGINT", () => { void app.close(); });
try { await app.run(); } finally { await app.close(); }
```

Set `SLACK_BOT_TOKEN` and `SLACK_APP_TOKEN`. `SLACK_TEAM_ID` and
`SLACK_BOT_USER_ID` are optional; otherwise Slack authentication discovers them.
The handler can await `context.reply(text)`, `context.history({ limit: 30 })`,
`context.conversation.history()`, and `context.subscribe()`. Returning a string
sends one additional reply; return `undefined` when already replying manually.
`close()` stops intake, lets admitted turns finish within `shutdownTimeoutMs`
(20 seconds by default), then aborts remaining `context.signal` values. Handlers
must cooperate with cancellation, especially when they own child processes.
Calling `await app.close()` from a handler requests shutdown and returns without
waiting for that same handler; outside a handler it awaits cleanup. Handler errors
report through the optional `onError(error, context)` constructor callback and
intake continues. The default callback logs a fixed failure notice. Startup and
permanent transport failures reject `run()`.

Native capabilities include `replyRich`, `setStatus`, `uploadFiles`, explicit
`downloadAttachment`, and `mirror`. None downloads event attachments automatically.
Use `const status = await context.working()` and `await status.close()` in a
`finally` block to refresh and clear a transient indicator; `status.update(text)`
changes its label. Upload/download content uses `Uint8Array`/`Buffer` and is not
added to conversation history. The host remains responsible for authorizing
history, file access, mirroring, and thread subscriptions.

A conversation runs one handler at a time. By default a message that arrives
meanwhile is steered: it is added to history and returned once by
`context.steered()`; messages the handler never reads run afterwards as their own
turns. Pass `busy: "queue"`, `"ask"` (Queue/Steer buttons), or `"reject"` to
change this. `app.stop(conversationId)` or a button from `context.offerStop()`
sets `context.stopped` and resolves `context.waitStopped()`; stopping is
cooperative. `context.ask(text, choices)` shows Block Kit buttons, accepts only
the sender's press by default (`allowed` widens it), and rejects with a
`TimeoutError` when nobody answers. `context.stream()` shows the answer with
Slack's native stream in threads and posts it once elsewhere; call `finish()`,
or `abort()` on failure, and return `null`. Button presses need **Interactivity**
turned on in the Slack app. See [Live controls](../../docs/live-controls.md).

Native metadata is portable JSON. Node handlers execute on Node's event loop;
provider I/O and per-conversation ordering run in Rust. Different conversations
can run concurrently. The default history and deduplication store is in memory.

See `examples/node` for a Claude Agent SDK integration with conversation sessions,
cancellation, successful-result validation, and explicit tool permissions.

For repository development: `npm ci`, `npm run build`, `npm test`,
`npm run typecheck`, and `npm run smoke`. Building from source requires Rust
1.88+ and a platform linker. Prebuilt release packages use the native artifact
for the target OS/architecture; building successfully for one target does not
validate another. Browser, Bun, Deno, and Electron support is not claimed.
