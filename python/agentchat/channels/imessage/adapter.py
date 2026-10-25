"""Thin class and FFI entry point for the native IMessage adapter."""
from agentchat._native import load_imessage

_IMessageBase = load_imessage()


class IMessage(_IMessageBase):
    __init__ = _IMessageBase.initialize

# BlueBubbles stops "typing…" after about 10 seconds or when the reply arrives.
TYPING_INTERVAL = 5
# Stop refreshing a forgotten indicator after this many seconds.
TYPING_EXPIRY = 120
