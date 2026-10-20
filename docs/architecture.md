# Architecture

AgentChat connects messaging platforms to application-owned agent runners. Each
platform implements the `Channel` protocol and emits the same `Message` values.
The application passes channel instances directly to `AgentChat`; there is no
provider registry or framework superclass.

Rust owns the runtime, provider policy, transport orchestration and lifecycle.
Python supplies compatible declarations and native exports; JavaScript supplies
its typed interface and callback glue. See the [support guide](rust-core.md).

```text
provider event
    │
    ▼
channel adapter       decode payload, apply provider routing/subscription rules
    │ Message
    ▼
AgentChat             claim ID → lock conversation → append → dispatch
    │ MessageContext
    ▼
application handler   authorize sender → fetch context → invoke chosen agent
    │ reply
    ▼
channel adapter       deliver to the source destination → return Message
    │
    ▼
AgentChat             append the returned reply to local state
```

## Ownership

| Module | Owns |
| --- | --- |
| `src/agentchat-core` | Typed models, turn transitions, state contracts, claims/history, context operations |
| `src/agentchat` | Public Rust interface |
| `src/agentchat-slack` | Shared Slack policy, rendering, history, files, native HTTP and Socket Mode transport |
| `src/agentchat-providers` | Telegram, WhatsApp, YCloud, Discord, iMessage and Teams normalization, routing and validation |
| `src/bindings/python` | Native Python classes, foreign object ownership, original-task async execution and SDK I/O adapters |
| `src/bindings/node` | Node native addon, callback and lifecycle bridge |
| `python/agentchat` | Compatible imports, dataclasses, protocols, optional SDK imports and native aliases |
| `packages/node` | JavaScript/TypeScript public interface |


Core modules depend on shared models and channel contracts, never a concrete
provider or agent framework. Adapters do not import each other. Framework helpers
consume normalized messages and do not own the channel lifecycle.

The Rust `Turn` engine emits typed effects. The native Python binding executes them inline
against the existing `Channel` and `State` protocols, preserving custom async
context managers and opaque message metadata. The native runtime executes the
same effects for Rust and Node. Bindings do not implement a separate turn
algorithm or parse provider routes.

The core records an adapter's returned reply and does not independently validate
provider receipts. Adapters own delivery confirmation and error propagation.

`context.history()` uses native thread history when supported, otherwise local
state. Native history failures propagate rather than silently returning partial
context. `context.conversation.history()` always reads only local state. File
bytes and working indicators do not enter conversation history.

Applications own sender authorization, agent execution, durable work and delivery
recovery. Direct channel binding is also supported when an application already
owns deduplication, ordering and history; see the [Slack guide](slack.md#using-an-existing-agent-runner).

## Python interface containment

Existing module paths stay available below `python/agentchat/channels/<provider>`.
They expose native functions and classes and keep provider SDK imports optional.
No routing, rendering, dispatch, file-transfer algorithm, lifecycle implementation,
or fallback runtime lives in these Python files. Dataclasses and protocols remain
Python declarations to preserve their existing identity and structural API.

Provider policy belongs to the Rust provider crate. A Python SDK adapter converts
foreign values, invokes that policy and drives the existing SDK effects through
the native host coroutine bridge. The bridge awaits callbacks on the caller's
asyncio task, so task-local locks and context variables remain valid. Native
Rust and Node Slack use the same provider policies with the Rust HTTP transport.

## Python composition

The generic adapter boundary remains `Channel`. Within Slack, replace event
delivery independently of message behavior:

```text
SocketMode ────────────┐
                       ├── SlackEventSource ── Slack.handle_event ── AgentChat
application event feed ┘                           │
                                                  ├── subscriptions/history
                                                  └── replies/files/status
```

`Slack.run()` authenticates the bot and checks the configured workspace before
starting its source. The source calls the same `handle_event` method for every
payload, so normalization, subscription filtering, sender identity and replies
remain shared. `Slack.close()` and `is_connected()` delegate source lifecycle.
The native Socket Mode implementation owns SDK invocation, acknowledgement,
bounded event tasks and graceful shutdown.

Supply exactly one of `app_token` (the Socket Mode convenience) or `event_source`
(an application-owned source). A custom source requires no app token and does
not import or instantiate Socket Mode. See the [composition guide](slack.md#composing-event-sources).

| Replaceable concern | Injection point | Responsibility |
| --- | --- | --- |
| Incoming events | `Slack(event_source=...)` | Deliver authenticated Slack events; own input lifecycle |
| Slack Web API calls | `Slack(web_client=...)` | Provide the existing `SlackWebClient` operations |
| Followed threads | `Slack(thread_subscriptions=...)` | Persist or expire Slack thread subscriptions |
| Conversation state | `AgentChat(state=...)` | Claim events, serialize conversations, retain history |
| Agent execution | `@app.on_message` | Authorize sender and call the application's runner |

Event decoding never downloads files. Standalone event and rendering helpers can
be imported without provider SDKs. File transport helpers accept explicit request
callbacks and authorization guards. Applications retain HTTP signature checking,
durable queues, authorization and ambiguous-delivery recovery.

Raw event envelopes, acknowledgement and connection behavior are provider-specific;
they do not belong in the generic `Channel` contract. A new provider can use a
similar composition seam when it has multiple input mechanisms, without inheriting
Slack code or changing `AgentChat`.

## Import migration

This cleanup deliberately removes old Slack shortcut/helper imports. Update them
as follows; no hidden aliases are retained:

| Previous import | Current import |
| --- | --- |
| `from agentchat.channels import Slack, SlackUser` | `from agentchat.channels.slack import Slack, SlackUser` |
| `agentchat.channels.slack_files.slack_attachments` | `agentchat.channels.slack.events.slack_attachments` |
| `agentchat.channels.slack_files.download_slack_file` | `agentchat.channels.slack.files.download_slack_file` |
| `agentchat.channels.slack_media.upload_slack_files` | `agentchat.channels.slack.files.upload_slack_files` |
| `agentchat.channels.slack_media.rich_payload` | `agentchat.channels.slack.rendering.rich_payload` |
| `agentchat.channels.slack_mirror.mirror_payload` | `agentchat.channels.slack.mirroring.mirror_payload` |

Other rendering helpers (`text_blocks`), file helpers (`upload_bytes`,
`validate_destination`, `SlackRequest`, `DeliveryGuard`) and `read_mirror` are
available from the corresponding canonical modules above.

Root-level message and context imports are unchanged. Importing the core does
not import Slack, aiohttp or OpenAI Agents. Installation defaults are unchanged:
Slack dependencies remain installed with the base package, while OpenAI Agents
still uses the `openai` extra.

Discord, Teams, and iMessage use separate optional extras. WhatsApp, YCloud, and Telegram use
aiohttp from the base install. Requesting one provider does not import the others.
