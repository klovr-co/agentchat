# Adding a channel

A channel is an adapter for a messaging platform. Put portable normalization,
routing and delivery policy in a Rust provider module under `src/`. Implement its
transport using the Rust channel traits or a foreign SDK adapter under
`src/bindings/`. Add only public declarations and native exports below
`python/agentchat/channels/<provider>/`. Pass an instance to
`AgentChat(channels=[...])`; no dispatcher changes are required.

Keep provider semantics in its owning Rust module. A language binding must consume
that module's typed results without reimplementing parsing, validation or routing.
Existing applications can still supply custom Python channel implementations.

## Required contract

Implement the existing `Channel` protocol in `agentchat.channels.base`:

```python
class Channel(Protocol):
    name: str

    def bind(self, receiver: MessageReceiver) -> None: ...
    async def run(self) -> None: ...
    async def close(self) -> None: ...
    async def reply(self, source: Message, content: str) -> Message: ...
```

This is a structural interface: implementing these operations is sufficient;
inheriting from `Channel` is unnecessary. `MessageReceiver` is an asynchronous
callback taking `(channel, message)`. See the minimal fake adapter in
[`tests/support/channels.py`](../tests/support/channels.py) and the production
[`Slack` adapter](../src/bindings/python/src/providers/slack.rs) for implementations.

1. **Bind and run.** Store the receiver, then establish transport resources in
   `run()`. Emit accepted events with `await receiver(self, message)`. Keep SDK
   imports, token/environment setup and connection handling inside the provider.
2. **Normalize messages.** Convert native events to `Message` and `Sender`.
   Prefix IDs with the provider and include workspace/account identity where
   needed: message IDs must remain stable across retries and distinct across
   conversations and providers. Conversation IDs identify the ordering/history
   boundary. Preserve thread identity and the metadata needed to route a reply.
3. **Reply.** Deliver text to the source's trusted destination. Return a confirmed
   assistant `Message` in the same conversation; propagate transport errors.
   Do not invent successful receipts or retry ambiguous sends. Provider-specific
   deduplication guarantees can be documented separately.
4. **Close.** Release connection resources and drain or cancel background work.
   Define bounded shutdown behavior and test it with fake transport clients.

Generic `AgentChat` already claims duplicate event IDs, serializes each
conversation and records the adapter's returned replies. Keep those concerns out of provider
code. Applications still authorize the current sender before agent operations.

## Optional capabilities

Implement only the extra protocols the provider supports. The core detects them
structurally; no capability registry is needed.

| Protocol | Operation | Behavior when absent |
| --- | --- | --- |
| `ThreadHistoryChannel` | `thread_history()` | Context falls back to local history |
| `StatusChannel` | `set_status()` | Returns `False` |
| `RichReplyChannel` | `reply_rich()` | Raises `TypeError` |
| `FileUploadChannel` | `upload_files()` | Raises `TypeError` |
| `FileDownloadChannel` | `download_attachment()` | Raises `TypeError` |
| `MirroringChannel` | `mirror()` | Raises `TypeError` |
| `StreamingChannel` | `start_stream()`, `append_stream()`, `stop_stream()` | One reply when the stream finishes |
| `InteractiveChannel` | `send_prompt()`, `close_prompt()` | `ask()` raises `TypeError`; `busy="ask"` steers |
| `TaskProgressChannel` | `update_stream_tasks()` | `AnswerStream.task()` and `plan()` are ignored |
| `TaskStreamingChannel` | `StreamingChannel` and `TaskProgressChannel` | Task updates start or join the stream |
| `HomeChannel` | `publish_home()` | No home screen |
| `EphemeralChannel` | `send_ephemeral()` | Raises `TypeError`; never posted publicly instead |
| `DeletingChannel` | `delete()` | Raises `TypeError` |

Read exact signatures in [`channels/base.py`](../python/agentchat/channels/base.py).
Streaming and prompt buttons are described in [Live controls](live-controls.md).
Implement `EphemeralChannel` only when the provider has truly private notices.
A provider stop control (such as Slack's native stop button) arrives as a
`Message` in the conversation with `metadata["agentchat_stop"] = True`, sent by
the person who pressed it; it stops the running turn only when that person sent
the message the turn answers, and it never starts a turn or enters history.
Native history returns messages before the current turn; the context appends the
current message and bounds the result. History errors propagate. Rich content can
remain provider-specific. Normalize attachment references without downloading
them; file bytes and status indicators stay outside message history.

## Test an adapter

Add provider fixtures and tests under `tests/channels/<provider>/`. Use explicit
helpers under `tests/support/` instead of importing from other test modules.

The shared suite in [`tests/contracts/test_channel.py`](../tests/contracts/test_channel.py)
checks normalized inbound identity, confirmed reply receipts, dispatch,
deduplication and failure propagation. Add a harness fixture for your provider in
[`tests/contracts/conftest.py`](../tests/contracts/conftest.py) to run these same
assertions against it. Each harness adapts native event injection and outbound
transport inspection; the contract tests remain provider-independent.

The new gateway/HTTP adapters also run the parametrized suite in
[`test_providers.py`](../tests/contracts/test_providers.py), covering receipts,
dispatch/deduplication, absent capabilities, queue limits, and real lifecycle
cleanup. Its fixtures use helpers from `tests/support/<provider>.py`. Add a new
provider there when using the bounded background-dispatch helper; its errors
are logged asynchronously instead of escaping the event callback.

Also cover provider-specific workspace checks, DM/thread routing, subscription
rules, malformed events, pagination, socket acknowledgement, queue limits and
shutdown where applicable. Capability tests should exercise unsupported features,
failed/ambiguous sends and cancellation. Use injected transports instead of live
credentials in the test suite.

```bash
ruff check .
python -m pytest tests/contracts tests/channels
python -m pytest
```

Finally add a runnable example and a provider setup guide. Keep the default
package import free of the provider SDK. If adding dependencies or optional
extras, test the installed package as well as the editable checkout. Provider exports belong in their own package, keeping `agentchat.channels`
limited to shared contracts. Adding a provider should not change core runtime
modules, the shared namespace or another adapter.


## Compose transports when a provider needs them

Keep the generic `Channel` contract focused on normalized messages. If one provider
supports multiple ways to receive native events, inject its event source into the
adapter and keep parsing/delivery shared. Slack demonstrates this with
`SlackEventSource` in `channels/slack/transport.py` and the default `SocketMode`
implementation in `src/bindings/python/src/providers/slack_socket.rs`.

A source implements `run(receiver)`, `close()` and `is_connected()`. It owns
transport resources and must stop intake and clean up when closed or cancelled.
It supplies provider events to the adapter callback; it does not reimplement
normalization, conversation state or agent execution. Authentication of an HTTP
request and durable acknowledgement remain the host source's responsibility.

The Slack source tests exercise the same adapter with an injected event queue,
including duplicate messages, subscribed follow-ups and replies. This demonstrates
composition without adding a provider registry or requiring every channel to
adopt Slack's transport interface.
