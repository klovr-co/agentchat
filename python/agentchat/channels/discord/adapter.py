"""Thin class and FFI entry point for the native Discord adapter."""
from agentchat._native import load_discord

_DiscordBase = load_discord()


class Discord(_DiscordBase):
    __init__ = _DiscordBase.initialize

# Discord clears "typing…" after about ten seconds; refresh it a little sooner.
TYPING_INTERVAL = 8
# Stop refreshing a forgotten indicator after this many seconds.
TYPING_EXPIRY = 120
