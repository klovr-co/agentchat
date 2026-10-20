# Public compatibility signatures; update with the corresponding Rust binding API.
from collections.abc import Mapping

def text_payload(
    content: str, *, sender: str, recipient: str, message_id: str, external_id: str | None = ...
) -> dict: ...
def template_payload(
    *,
    sender: str,
    recipient: str,
    name: str,
    language_code: str,
    components: list[Mapping[str, object]] | None = ...,
    external_id: str | None = ...,
) -> dict: ...
