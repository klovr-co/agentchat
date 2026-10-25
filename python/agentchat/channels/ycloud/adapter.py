"""Compatibility exports and value types for the native YCloud WhatsApp adapter."""
from collections.abc import Awaitable, Callable
from typing import NamedTuple

import aiohttp  # noqa: F401 -- public transport injection compatibility

from agentchat._native import YCloud as YCloud
from agentchat._native import ycloud_error_init
from agentchat.models import Message

API_ROOT = "https://api.ycloud.com/v2"


class WhatsAppApiError(NamedTuple):
    """Meta's original error details, as YCloud forwards them."""

    code: str | None
    message: str | None
    type: str | None
    error_data: object


class YCloudStatus(NamedTuple):
    """One `whatsapp.message.updated` event. Updates arrive unordered; apply forward only."""

    event_id: str | None
    status: str
    timestamp: str | None
    message_id: str | None
    wamid: str | None
    external_id: str | None
    recipient: str | None
    conversation_id: str | None
    error_code: str | None
    error_message: str | None
    error_kind: str | None
    whatsapp_api_error: WhatsAppApiError | None


# Business App echoes arrive as `Message(role="assistant", metadata={"echo": True, ...})`.
YCloudEvent = YCloudStatus | Message
YCloudEventHandler = Callable[[YCloud, YCloudEvent], Awaitable[object]]


class YCloudTemplate(NamedTuple):
    name: str
    language: str
    category: str | None
    status: str | None
    body: str | None
    components: list


class YCloudError(RuntimeError):
    """A failed YCloud call; branch on `kind`, never on the message text."""

    WINDOW_EXPIRED = "window_expired"
    TEMPLATE_PARAMETERS = "template_parameters"
    TEMPLATE_NOT_FOUND = "template_not_found"
    TEMPLATE_UNAVAILABLE = "template_unavailable"
    BALANCE_INSUFFICIENT = "balance_insufficient"
    UNAUTHORIZED = "unauthorized"
    RATE_LIMITED = "rate_limited"
    REJECTED = "rejected"
    UNCERTAIN = "uncertain"
    MEDIA_TOO_LARGE = "media_too_large"
    MEDIA_INTEGRITY = "media_integrity"
    MEDIA_UNAVAILABLE = "media_unavailable"

    def __init__(
        self,
        description: str,
        *,
        kind: str,
        status: int | None = None,
        code: str | None = None,
        whatsapp_code: str | None = None,
        request_id: str | None = None,
    ) -> None:
        ycloud_error_init(
            self, description, kind=kind, status=status, code=code,
            whatsapp_code=whatsapp_code, request_id=request_id,
        )

# WhatsApp clears "typing…" after 25 seconds or when the reply arrives.
TYPING_INTERVAL = 20
# Stop refreshing a forgotten indicator after this many seconds.
TYPING_EXPIRY = 120
