# Public compatibility signatures; update with the corresponding Rust binding API.
import re
from collections.abc import Mapping

from agentchat.models import Attachment, Message

FILE_ID: re.Pattern[str]
ACK_DEADLINE: float

def responds(payload: Mapping[str, object]) -> bool: ...

def slack_attachments(files: object, *, limit: int = ...) -> tuple[Attachment, ...]: ...
def _filename(value: str) -> str: ...
def to_message(
    payload: Mapping[str, object],
    event: Mapping[str, object],
    *,
    bot_user_id: str | None,
    workspace_id: str | None,
    channel_name: str = ...,
) -> Message | None: ...
