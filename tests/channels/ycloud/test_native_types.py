import weakref
from unittest.mock import AsyncMock

import pytest
from aiohttp import web
from aiohttp.test_utils import TestClient, TestServer

from agentchat.channels.ycloud import YCloud


@pytest.mark.asyncio
async def test_native_ycloud_preserves_subclasses_weakrefs_and_webhook_overrides():
    class CustomYCloud(YCloud):
        def __init__(self, config):
            super().__init__(
                phone_number=config["phone"], api_key="key", webhook_secret="secret",
            )

        async def _receive(self, request):
            return web.Response(text="custom receiver")

    channel = CustomYCloud({"phone": "+15550001111"})
    reference = weakref.ref(channel)
    assert reference() is channel
    assert type(channel) is CustomYCloud
    channel.reply = replacement = AsyncMock(return_value="replacement")
    assert channel.reply is replacement
    try:
        async with TestClient(TestServer(channel.create_app())) as client:
            response = await client.post("/webhooks/ycloud")
            assert await response.text() == "custom receiver"
    finally:
        await channel.close()
