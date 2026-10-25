"""Compatibility exports for the native WhatsApp adapter."""
import aiohttp  # noqa: F401 -- public transport injection compatibility

from agentchat._native import WhatsApp as WhatsApp

GRAPH_ROOT = "https://graph.facebook.com"

# WhatsApp clears "typing…" after 25 seconds or when the reply arrives.
TYPING_INTERVAL = 20
# Stop refreshing a forgotten indicator after this many seconds.
TYPING_EXPIRY = 120
