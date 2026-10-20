"""YCloud WhatsApp adapter and provider-local helpers, loaded on demand."""

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from agentchat.channels.ycloud.adapter import (
        WhatsAppApiError,
        YCloud,
        YCloudError,
        YCloudStatus,
        YCloudTemplate,
    )

__all__ = ["WhatsAppApiError", "YCloud", "YCloudError", "YCloudStatus", "YCloudTemplate"]


def __getattr__(name: str) -> object:
    if name not in __all__:
        raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
    try:
        from agentchat.channels.ycloud import adapter
    except ModuleNotFoundError as exc:
        if exc.name and exc.name.split(".")[0] == "aiohttp":
            raise ModuleNotFoundError(
                "Install YCloud support with: pip install 'agentchat[ycloud]'",
                name=exc.name,
            ) from exc
        raise
    return getattr(adapter, name)


def __dir__() -> list[str]:
    return sorted(set(globals()) | set(__all__))
