# Public compatibility signatures; update with the corresponding Rust binding API.
from agentchat.models import Message

def normalize(payload: object, *, phone_number: str) -> tuple[Message, ...]: ...
