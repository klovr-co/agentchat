from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass, field
from types import MappingProxyType
from typing import TYPE_CHECKING, Literal

if TYPE_CHECKING:
    from agentchat.context import Conversation, MessageContext

__all__ = [
    "Answer", "Attachment", "Choice", "Conversation", "DownloadedFile", "Message",
    "MessageContext", "Prompt", "RichReply", "Role", "Sender", "UploadFile", "UploadedFile",
]

Role = Literal["user", "assistant"]


@dataclass(frozen=True, slots=True)
class Sender:
    id: str
    display_name: str | None = None


@dataclass(frozen=True, slots=True)
class Attachment:
    """Incoming provider file reference; metadata is advisory, never a download URL."""

    id: str
    name: str | None = None
    media_type: str | None = None
    size: int | None = None


@dataclass(frozen=True, slots=True)
class DownloadedFile:
    """Explicitly fetched bytes; the host owns content validation and storage."""

    id: str
    name: str
    content: bytes = field(repr=False)
    media_type: str | None = None


@dataclass(frozen=True, slots=True)
class Message:
    id: str
    conversation_id: str
    channel: str
    sender: Sender
    text: str
    role: Role
    metadata: Mapping[str, object] = field(default_factory=lambda: MappingProxyType({}))
    thread_id: str | None = None
    addressed: bool = True
    attachments: tuple[Attachment, ...] = ()


@dataclass(frozen=True, slots=True)
class RichReply:
    """Fallback text and explicitly provider-compatible rich content."""

    text: str
    blocks: tuple[Mapping[str, object], ...] = ()
    attachments: tuple[Mapping[str, object], ...] = ()


@dataclass(frozen=True, slots=True)
class Choice:
    """One button of a prompt. `id` is returned in the answer; people see `label`."""

    id: str
    label: str
    style: Literal["default", "primary", "danger"] = "default"


@dataclass(frozen=True, slots=True)
class Prompt:
    """A question with buttons, as AgentChat hands it to a channel to show."""

    id: str
    text: str
    choices: tuple[Choice, ...]


@dataclass(frozen=True, slots=True)
class Answer:
    """The button someone pressed, and who pressed it."""

    prompt: str
    choice: str
    label: str
    sender: Sender


@dataclass(frozen=True, slots=True)
class UploadFile:
    """File bytes selected and authorized by the host application."""

    filename: str
    content: bytes
    title: str | None = None


@dataclass(frozen=True, slots=True)
class UploadedFile:
    """Confirmed provider file identity; not a conversation message."""

    id: str
    permalink: str | None = None


def __getattr__(name: str) -> object:
    # Preserve the original imports without making value types depend on the runtime.
    if name in {"Conversation", "MessageContext"}:
        from agentchat import context

        return getattr(context, name)
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


def __dir__() -> list[str]:
    return sorted(set(globals()) | set(__all__))
