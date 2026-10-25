"""Event-source contract for composing Slack with host-owned transports."""
from __future__ import annotations

from collections.abc import Awaitable, Callable, Mapping
from typing import Protocol

SlackEventReceiver = Callable[[Mapping[str, object]], Awaitable[Mapping[str, object] | None]]


class SlackEventSource(Protocol):
    """Deliver authenticated Slack payloads to the shared Slack adapter.

    Payloads are Events API envelopes, interactive payloads (button presses, view
    submissions, shortcuts) and slash commands. The receiver returns the response
    body for payloads whose acknowledgement may carry one: view submissions,
    options requests and slash commands (see
    ``agentchat.channels.slack.events.responds``). For those, await the receiver
    before responding; it returns within ``events.ACK_DEADLINE`` seconds.
    Acknowledge everything else first, as before.

    ``run`` stays active until shutdown and propagates startup/transport errors.
    The source owns acknowledgement, backpressure and accepted background work;
    ``close`` must stop intake and drain or cancel that work within a bound. It
    must be safe to close before startup or more than once. Host HTTP sources
    verify Slack signatures before delivery; the receiver does not authenticate
    raw requests. A source instance is owned by one Slack adapter.
    """

    async def run(self, receiver: SlackEventReceiver) -> None: ...

    async def close(self) -> None: ...

    async def is_connected(self) -> bool:
        """Whether the source is currently ready to deliver authenticated events."""
        ...
