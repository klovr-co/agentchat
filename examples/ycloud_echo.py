"""Echo messages after completing docs/ycloud.md; no model API key required."""

import asyncio
import logging

from agentchat import AgentChat, MessageContext
from agentchat.channels.ycloud import YCloud


async def main() -> None:
    app = AgentChat(channels=[YCloud.from_env()])

    @app.on_message
    async def respond(context: MessageContext) -> str:
        # Authorize the sender here before replacing this echo with an agent.
        return context.message.text or "Received your attachment."

    try:
        await app.run()
    finally:
        await app.close()


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    asyncio.run(main())
