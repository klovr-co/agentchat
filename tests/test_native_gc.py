"""Construction must remain safe when CPython collects inside object_new."""
import os
import subprocess
import sys
import textwrap


def test_native_instances_can_be_collected_during_dictionary_allocation():
    # A subprocess turns a native GC crash into a clear regression failure, and
    # avoids changing the collector configuration in the main pytest process.
    script = textwrap.dedent('''
        import asyncio
        import gc
        from unittest.mock import AsyncMock
        from agentchat import AgentChat
        from agentchat._delivery import DeliveryTasks
        from agentchat.activity import WorkingStatus
        from agentchat.context import Conversation, MessageContext
        from agentchat.state import MemoryState
        from agentchat.channels.discord import Discord
        from agentchat.channels.imessage import IMessage
        from agentchat.channels.slack import Slack
        from agentchat.channels.slack.socket_mode import SocketMode
        from agentchat.channels.teams import Teams
        from agentchat.channels.telegram import Telegram
        from agentchat.channels.telegram.transport import TelegramBotClient
        from agentchat.channels.whatsapp import WhatsApp
        from agentchat.channels.ycloud import YCloud
        from tests.support.channels import FakeChannel, incoming
        from tests.support.discord import DiscordClient
        from tests.support.imessage import BubblesClient, SocketClient
        from tests.support.receipts import Receipt
        from tests.support.slack import FakeSlackClient
        from tests.support.teams import APP_ID, TENANT_ID, teams_adapter
        from tests.support.telegram import TelegramClient

        async def exercise():
            gc.set_threshold(1, 1, 1)
            for index in range(30):
                client = DiscordClient(Receipt(123))
                adapter, *_ = teams_adapter(Receipt())
                slack_client = FakeSlackClient()
                channels = [
                    Discord(bot_token="fake", client=client, send_client=client),
                    IMessage(server_url="http://localhost:1234", password="fake",
                             instance_id="mac", client=BubblesClient(Receipt()),
                             socket_client=SocketClient()),
                    Teams(app_id=APP_ID, tenant_id=TENANT_ID, app_password="fake", adapter=adapter),
                    Slack(bot_token="fake", app_token="fake", web_client=slack_client),
                    Telegram(bot_token="99:fake", client=TelegramClient()),
                    WhatsApp(phone_number_id="123", access_token="token",
                             app_secret="secret", verify_token="verify"),
                    YCloud(phone_number="+15550001111", api_key="key",
                           webhook_secret="secret"),
                ]
                transport = TelegramBotClient("99:fake")
                socket = SocketMode(app_token="fake", web_client=slack_client)
                fake = FakeChannel()
                state = MemoryState()
                app = AgentChat(channels=[fake], state=state)
                context = MessageContext(app, fake, incoming(str(index), "hello"))
                conversation = Conversation(app, "conversation")
                delivery = DeliveryTasks(64, 20)
                working = WorkingStatus(AsyncMock(), "working")
                # Native dictionaries must retain Python's dynamic attributes.
                for instance in [*channels, transport, socket, state, app,
                                 context, conversation, delivery, working]:
                    instance.gc_regression = index
                    assert instance.gc_regression == index
                for channel in channels:
                    await channel.close()
                await transport.close()
                await socket.close()
                await delivery.close()
                gc.collect()
        asyncio.run(exercise())
    ''')
    result = subprocess.run(
        [sys.executable, "-X", "faulthandler", "-c", script],
        text=True,
        capture_output=True,
        timeout=60,
        env={**os.environ, "PYTHONMALLOC": "debug"},
    )
    assert result.returncode == 0, result.stdout + result.stderr


def test_public_native_interfaces_preserve_subclassing_and_weak_references():
    import weakref
    from unittest.mock import AsyncMock

    from agentchat import AgentChat
    from agentchat._delivery import DeliveryTasks
    from agentchat.activity import WorkingStatus
    from agentchat.channels.slack import MemoryThreadSubscriptions, Slack
    from agentchat.channels.slack.socket_mode import SocketMode
    from agentchat.context import Conversation, MessageContext
    from agentchat.state import MemoryState
    from tests.support.channels import FakeChannel, incoming
    from tests.support.slack import FakeSlackClient

    fake = FakeChannel()
    app = AgentChat(channels=[fake])
    specifications = [
        (AgentChat, (), {"channels": [fake]}),
        (MemoryState, (), {}),
        (DeliveryTasks, (64, 20), {}),
        (WorkingStatus, (AsyncMock(), "working"), {}),
        (Conversation, (app, "conversation"), {}),
        (MessageContext, (app, fake, incoming("subclass", "hello")), {}),
        (SocketMode, (), {"app_token": "fake", "web_client": FakeSlackClient()}),
        (Slack, (), {"bot_token": "fake", "app_token": "fake", "web_client": FakeSlackClient()}),
        (MemoryThreadSubscriptions, (), {}),
    ]
    def initializer(base, args, kwargs):
        def initialize(self, configuration):
            base.__init__(self, *args, **kwargs)
            self.configuration = configuration
        return initialize

    for base, args, kwargs in specifications:
        derived = type(f"Custom{base.__name__}", (base,),
                       {"__init__": initializer(base, args, kwargs)})
        instance = derived("custom positional configuration")
        assert type(instance) is derived
        assert instance.configuration == "custom positional configuration"
        assert weakref.ref(instance)() is instance
