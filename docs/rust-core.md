# Rust core and language interfaces

AgentChat uses a shared Rust turn engine with Python, Node.js/TypeScript and Rust
interfaces. The application still owns its agent framework, sender authorization,
model configuration and agent session storage.

## Python compatibility

Existing imports, dataclasses, channel objects, decorators and async context
methods keep working:

```python
from agentchat import AgentChat
from agentchat.channels.slack import Slack

app = AgentChat(channels=[Slack.from_env()])

@app.on_message
async def respond(context):
    return context.message.text
```

All AgentChat runtime and provider implementation code lives in Rust under `src/`.
The `python/agentchat/` package contains compatible imports, dataclass and protocol
declarations, optional SDK imports, and direct native exports. There is no Python
dispatcher, provider implementation, or fallback backend. The former
`backend="python"` option now raises an explicit error; `backend="rust"` remains
accepted. `AGENTCHAT_BACKEND` no longer selects an implementation.

The native Python binding awaits foreign SDK and application callbacks on the
original asyncio task. This preserves custom state locks, context variables,
exception identity, cancellation, subclass overrides, and application-owned
Python objects. Existing injected SDK clients and event sources remain supported.
Third-party Python SDKs still perform transport I/O where required by the existing
interface; AgentChat owns their sequencing, routing and lifecycle in Rust.

Native coroutine objects must be awaited, cancelled through their owning task, or
explicitly closed if abandoned before scheduling. Completion, cancellation and
`close()` release captured Python objects; closing a suspended coroutine also runs
its context-manager cleanup. A never-awaited coroutine retained in a cycle through
its own arguments or opaque application metadata cannot currently be reclaimed by
Python's cycle collector, because those captures live in a Rust future. Avoid
storing pending coroutine objects in their own inputs; use an asyncio task for
scheduled work and await its completion or cancellation.

Installing from a wheel needs no Rust toolchain. Installing this checkout or a
source distribution builds `agentchat._native` with maturin and requires Rust.
The minimum Python version is 3.11, using `abi3-py311` wheels. Free-threaded Python
and alternative interpreters are not qualified by this release.

## Node and Claude Agent SDK

Build the package from this checkout:

```bash
cd packages/node
npm ci
npm run build
npm test
npm run smoke
```

See the [Node package](../packages/node/README.md) and
[Claude example](../examples/node/README.md) for the JavaScript API and a Slack bot
using `@anthropic-ai/claude-agent-sdk`. The Node package is a native napi-rs addon;
it needs no Python installation. Node 22 and 24 are the initial CI targets.
Browser/edge runtimes, Bun and Deno are outside this initial support matrix.

Claude executes in the Node handler. Map each AgentChat conversation ID to a
Claude session ID, resume it for subsequent turns, and return the successful
final result for automatic delivery. AgentChat history and Claude sessions are
separate stores. An uncertain Slack reply does not warrant rerunning Claude or
replaying a send. Forward cancellation into the Claude SDK and close AgentChat
explicitly during application shutdown.

## Rust interface

`src/agentchat` is the public Rust facade. Enable its `slack` feature for the
native provider. `src/agentchat-core` owns typed models, state contracts,
conversation ordering, context operations and the turn engine. Custom channels
implement the open `Channel` trait; they do not need registration in a closed
provider enum. Rust applications use their own Tokio runtime.

Provider crates own normalization, route validation, authentication, transports
and confirmed receipts. Language bindings do not parse Slack routing fields or
implement independent turn algorithms. Shared domain results use named structs
and enums. IDs stay strings across the public interfaces to avoid JavaScript
integer precision loss.

## Current channel support

| Channel | Python interface | Node and Rust interfaces |
| --- | --- | --- |
| Slack | Rust implementation, compatible Python SDK injection | Native Rust transport |
| Telegram | Rust implementation, Python SDK/HTTP I/O | Transport binding pending |
| WhatsApp | Rust implementation, Python HTTP I/O | Transport binding pending |
| YCloud | Rust implementation, Python HTTP I/O | Transport binding pending |
| Discord | Rust implementation, Python SDK/HTTP I/O | Transport binding pending |
| iMessage | Rust implementation, Python SDK/HTTP I/O | Transport binding pending |
| Teams | Rust implementation, Python SDK I/O | Transport binding pending |

`src/agentchat-providers` owns portable policies for the five non-Slack providers.
Those policies are available to Rust callers; their complete channel transports
are currently exposed through the Python interface. Slack's complete transport
is also available to Node and Rust. Adding another language transport must consume
the existing Rust policy rather than copy provider rules into a binding.

Claims/history are in-process and unbounded, matching the previous default.
Claims survive handler failures within that state and disappear on restart.
There is no durable queue or exactly-once guarantee. Native Slack makes a single
write attempt and only records confirmed receipts; rate-limit scheduling and
uncertain-delivery recovery remain application concerns. Existing Python
provider-specific behavior is retained.

## Checks and binary qualification

Run these checks before proposing a change:

```bash
python -m pip install -e '.[dev]'
ruff check .
MYPYPATH=python mypy --ignore-missing-imports --follow-imports silent scripts/python-typing-smoke.py
python -m pytest
cargo fmt --all --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Public Python modules ship adjacent `.pyi` declarations and a `py.typed` marker.
The declarations were extracted from the pre-port Python API; binding owners must
update them alongside Rust signature changes, including return types and public
instance attributes. Keep stubs declaration-only. The typing smoke checks all six
channels against the shared protocol and checks context, history and activity
types. Installed-wheel checks verify that typing files are packaged.

Node and Claude example commands are in their respective READMEs. Tests use
injected/local Slack HTTP and WebSocket servers and a fake Claude stream, so they
do not send messages to a workspace or require model credentials. A live echo
check with the application's real Slack installation and Claude credentials is
still needed before deploying that application.

CI exercises the native Python interface, the Rust contracts, Node/Claude flows and
installed packages. Binary jobs build and smoke-test Linux glibc x64/arm64,
macOS x64/arm64 and Windows x64. These are qualification targets: support is
confirmed by passing jobs, not merely by successful cross-compilation. Source
distribution installation is checked independently. Musl and other targets
require separate qualification.

Coordinate future PyPI, npm and Cargo releases from the same source revision and
version. Build and test the complete binary set before publishing; no workflow
in this change publishes to a registry automatically. Include the core/package
version and platform when reporting a binding issue. Changes to shared behavior
must pass all language contract tests; new provider support must update this
matrix and include setup documentation and an example.
