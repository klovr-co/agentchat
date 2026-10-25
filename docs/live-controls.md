# Live status, streaming, buttons, and busy conversations

Four controls let people follow and steer an agent while it works. Each works on
every channel; how it looks depends on what the provider can show.

| | Slack | Telegram | Discord | Teams | WhatsApp and YCloud | iMessage |
| --- | --- | --- | --- | --- | --- | --- |
| Live status (`working()`) | Status text in the thread | "typing…" | "typing…" | "typing…" | "typing…" (marks the message read) | "typing…" (needs the BlueBubbles Private API) |
| Streamed answer (`stream()`) | Native stream in threads | Edits one message | Edits one message | Native stream in one-to-one chats, otherwise edits one message | One reply when finished | One reply when finished |
| Task and plan progress (`reply.task()`, `reply.plan()`) | Task cards or a plan block in the stream | Ignored | Ignored | Ignored | Ignored | Ignored |
| Buttons (`ask()`) | Block Kit buttons | Inline keyboard | Message buttons | Adaptive Card | Reply buttons, or a list for more than three or longer choices | None: `ask()` raises `TypeError` |
| Stop / Queue / Steer | Buttons, and Slack's native stop button | Buttons | Buttons | Buttons | Buttons | `app.stop()` and `busy=` only |

The Python interface supports every channel above. The Node and Rust interfaces
use the same Rust runtime for Stop, Queue, Steer, prompts, and streaming; their
native transport is Slack (see [Node and Rust](#node-and-rust)).

Without the Private API, iMessage `set_status()` returns `False`. Its streamed
answers arrive as one reply, `ask()` raises `TypeError`, and `busy="ask"` steers.

## A handler that uses all four

```python
from agentchat import AgentChat, Choice

app = AgentChat(channels=[...])  # busy="steer" is the default


@app.on_message
async def respond(context):
    async with context.working("is reading the repository…"):
        await context.offer_stop()
        answer = await context.ask(
            "Delete the build directory?",
            [Choice("yes", "Delete", "danger"), Choice("no", "Keep")],
            timeout=600,
        )
        if answer.choice != "yes":
            return "Kept the build directory."

        # run_agent and steer_agent stand for your agent framework's calls.
        async with context.stream() as reply:
            async for chunk in run_agent(context.message.text):
                if context.stopped:
                    await reply.append("\n\nStopped.")
                    break
                for message in context.steered():
                    steer_agent(message.text)
                await reply.append(chunk)
```

## Live status

`context.working(status)` shows the status while the block runs and clears it
afterwards, even when the handler fails. Providers that clear "typing…" after a
few seconds get it again automatically until the block ends, the reply arrives,
or 120 seconds pass. `context.set_status(status)` sets it once; `""` clears it.

## Streamed answers

`context.stream()` shows the answer while it is written. Each `append()` adds
text; at most one update is sent per `stream_interval` (Slack and Discord 1 s,
Teams 1.5 s, Telegram 2 s), and the rest is sent when the block ends. The
finished answer is confirmed as ordinary messages and added to conversation
history. A long answer continues in a new message at the provider's limit
(Discord 2,000, Telegram 4,096, Slack 12,000 UTF-16 units), after a line or
word break where one is near. Slack finalizes a stream 10 minutes after it starts
with what was streamed so far; later updates to that stream fail.

### Task and plan progress

`await reply.task(id, title, status, details=None, output=None)` shows a step
next to the answer; `status` is `"pending"`, `"in_progress"`, `"complete"` or
`"error"`. A later call with the same `id` updates that task; `details` and
`output` add text to it, so send each piece once. `await reply.plan(title)`
groups the tasks under a plan. Updates are sent with the text, at most once per
interval.

```python
async with context.stream() as reply:
    await reply.plan("Fixing the build")
    await reply.task("logs", "Read the build logs")
    await reply.append(await diagnose())
    await reply.task("logs", "Read the build logs", "complete", output="1 error")
```

On Slack the updates are native `task_update` and `plan_update` chunks: tasks
show as a plan block when a plan is named, otherwise as cards in the answer.
Slack fixes a stream's form when it starts, so call `plan()` or `task()` before
the first `append()`; a stream that started with text alone cannot add tasks.
If Slack refuses task updates (for example an older workspace), the answer
streams as plain text without them and the handler carries on. Task titles,
details and output are escaped so they cannot create mentions or links.

Other channels ignore task updates. They are progress, not answer text, and
turning them into text would put stale steps into the confirmed answer and its
history; an app that wants them in the text can append them itself.

Return `None` from the handler after streaming; a returned string is sent as
another reply. If the handler raises inside the block, the stream is closed with
the text so far and nothing is added to history. Stream errors propagate; nothing
is retried or sent twice. Channels without streaming, and Slack messages outside
a thread, get one reply when the block ends.

## Buttons and approvals

`context.ask(text, choices)` shows `text` with one button per choice (at most 10)
and returns an `Answer` with the `choice` ID, its `label`, and the `sender` who
pressed it. Plain strings become choices with IDs `"1"`, `"2"`, ….

A press counts only if it is:

- from the same conversation as the prompt,
- one of the prompt's choices,
- from someone allowed to answer: by default the sender of the message the
  prompt is about. Pass `allowed=["U123", ...]` or `allowed="anyone"` to widen it,
- the first answer; the prompt closes after one.

Anything else is ignored. Prompts fail closed: a channel without buttons raises
`TypeError`, and a prompt nobody answers within `timeout` seconds (default 600)
raises `TimeoutError`. Neither is ever an approval. After an answer the buttons
are replaced by the choice and who made it; an expired prompt just loses its
buttons.

WhatsApp and YCloud cannot edit sent messages, so their buttons stay after an
answer, but later presses do nothing. `WhatsApp(prompt_style="reactions")` (or
`WHATSAPP_PROMPT_STYLE=reactions`) asks for a reaction instead: the prompt lists
an emoji for each choice (👍 for one, 👍 and 👎 for two, 1️⃣ to 🔟 for more), and
the first matching reaction from someone allowed to answer is the answer. Skin
tones don't matter and other reactions are ignored.

Presses are not added to conversation history and never start a turn. Prompts
live in memory and end when the process restarts.

## Busy conversations

A conversation runs one handler at a time. `AgentChat(busy=...)` decides what
happens to a message that arrives while its handler is still running:

| `busy` | What happens |
| --- | --- |
| `"steer"` (default) | The message is added to history and handed to the running handler through `context.steered()`. Messages the handler never reads run afterwards as their own turns, in order, so nothing is lost. |
| `"queue"` | The message waits and runs as the next turn. `context.steered()` stays empty. |
| `"ask"` | The sender gets two buttons: **Add to current request** (steer) or **Answer after it** (queue). If they don't choose before the running turn ends, the message is queued. Channels without buttons steer. |
| `"reject"` | The message is dropped and not added to history. |

Steer is new in this release; earlier versions queued. Use `busy="queue"` for the
old behaviour.

`context.steered()` returns each steered message once. Read it at points where
your agent can take new input, for example between tool calls.

## Stop

`app.stop(conversation_id)` asks that conversation's running handler to stop and
returns `False` when none is running. `context.offer_stop()` shows a Stop button
that does the same; by default only the sender of the request can press it, and
it goes away when the turn ends. Channels without buttons return `None`.

On Slack, the native stop button that Slack shows while an agent is working does
the same as the Stop button: it arrives as a `agent_session_stopped` event and
stops the running turn if pressed by the person who started it. AgentChat then
clears the thread's working status, since Slack leaves that to the app. The event
also reaches `@slack.on_event("agent_session_stopped")`, where an app can widen
who may stop by calling `app.stop()` itself. Subscribe to the event to get the
button; see [Slack](slack.md#agent-sessions-and-the-native-stop-button).

Stopping is cooperative: `context.stopped` becomes `True` and
`await context.wait_stopped()` returns. The handler decides how to wind down, for
example by cancelling the agent's run and returning what it has. Nothing is
cancelled for it.

## Provider setup

- **Slack:** turn on **Interactivity & Shortcuts** in the app settings so button
  presses reach Socket Mode. Streaming uses `chat.startStream`, which works in
  threads.
- **Discord:** button presses arrive over the gateway; no extra intents are
  needed. A custom `send_client` needs `edit_text(channel_id, message_id,
  payload)` to stream and close prompts, and `trigger_typing(channel_id)` for
  status.
- **Teams:** prompt cards use `Action.Submit`. In channel threads a press is
  matched to the thread from the conversation ID's `;messageid=` suffix.
- **WhatsApp and YCloud:** prompts are free-form messages, so they need an open
  customer-service window. Reaction prompts need the WhatsApp webhook's
  `messages` field, which includes reactions.

## Node and Rust

The Node package exposes the same controls with JavaScript names:

```ts
const app = new AgentChat({ channels: [Slack.fromEnv()], busy: "steer" });

app.onMessage(async (context) => {
  await context.offerStop();
  const answer = await context.ask("Delete the build directory?", [
    { id: "yes", label: "Delete", style: "danger" },
    { id: "no", label: "Keep" },
  ], { timeoutMs: 600_000 });
  if (answer.choice !== "yes") return "Kept the build directory.";

  const reply = context.stream();
  try {
    for await (const chunk of runAgent(context.message.text)) {
      if (context.stopped) break;
      for (const message of context.steered()) steerAgent(message.text);
      await reply.append(chunk);
    }
    await reply.finish();
  } catch (error) {
    await reply.abort();
    throw error;
  }
  return null;
});
```

`reply.task(id, title, status, { details, output })` and `reply.plan(title)`
show progress. An unanswered prompt rejects with a `DOMException` named
`TimeoutError`.
`app.stop(conversationId)` and `await context.waitStopped()` work as in Python.

In Rust, `AgentChat::new().with_busy(Busy::Steer)` sets the policy, and the
handler context has `steered()`, `stopped()`, `wait_stopped()`,
`ask(text, choices, AskOptions)`, `offer_stop(StopOptions)`, and
`stream(interval)`, whose `task(TaskUpdate)` and `plan(title)` show progress.
`ask` fails with `Error::Unsupported` without buttons and
`Error::Timeout` when nobody answers. Because unread steered messages run as
their own turns, `receive` takes a handler it can call more than once (`Fn`).
A custom `Channel` opts in by overriding `streaming()`, `start_stream`,
`append_stream`, and `stop_stream`; `stream_tasks()` and `update_stream_tasks`;
and `supports_prompts()`, `send_prompt`, and `close_prompt`.

## Writing a channel

See [Adding a channel](adding-a-channel.md) for `StreamingChannel` and
`InteractiveChannel`. A button must carry `agentchat.prompt_token(prompt.id,
choice.id)` (at most 52 bytes), and a press must arrive as a `Message` in the
prompt's conversation with `metadata["agentchat_answer"] = {"prompt": ...,
"choice": ...}`, sent by the person who pressed.
