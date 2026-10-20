"""Compatibility exports for the native YCloud WhatsApp adapter."""
import aiohttp  # noqa: F401 -- public transport injection compatibility

from agentchat._native import YCloud as YCloud

API_ROOT = "https://api.ycloud.com/v2"
