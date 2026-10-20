"""YCloud WhatsApp adapter and provider-local helpers, loaded on demand."""

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from agentchat.channels.ycloud.adapter import YCloud

__all__ = ["YCloud"]


def __getattr__(name: str) -> object:
    if name != "YCloud":
        raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
    try:
        from agentchat.channels.ycloud.adapter import YCloud
    except ModuleNotFoundError as exc:
        if exc.name and exc.name.split(".")[0] == "aiohttp":
            raise ModuleNotFoundError(
                "Install YCloud support with: pip install 'agentchat[ycloud]'",
                name=exc.name,
            ) from exc
        raise
    return YCloud


def __dir__() -> list[str]:
    return sorted(set(globals()) | set(__all__))
