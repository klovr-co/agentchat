"""Thin class and FFI entry point for the native Teams adapter."""
from agentchat._native import load_teams

_TeamsBase = load_teams()


class Teams(_TeamsBase):
    __init__ = _TeamsBase.initialize

# Teams clears "typing…" after about three seconds; refresh it that often.
TYPING_INTERVAL = 3
# Stop refreshing a forgotten indicator after this many seconds.
TYPING_EXPIRY = 120
