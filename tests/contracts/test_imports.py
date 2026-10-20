import subprocess
import sys

import pytest


def run_script(code):
    result = subprocess.run(
        [sys.executable, "-c", code], capture_output=True, text=True, check=False
    )
    assert result.returncode == 0, result.stderr


BLOCK_IMPORTS = """
import sys
from importlib.abc import MetaPathFinder
class Block(MetaPathFinder):
    def find_spec(self, fullname, path=None, target=None):
        if fullname.split('.')[0] in {'slack_sdk', 'agents', 'openai', 'discord',
                                     'microsoft_agents', 'socketio'}:
            raise ModuleNotFoundError('blocked optional dependency', name=fullname)
sys.meta_path.insert(0, Block())
"""


def test_core_and_namespaces_do_not_import_provider_or_framework_sdks():
    run_script(
        BLOCK_IMPORTS
        + """
from agentchat import AgentChat, Message, MessageContext, Sender
from agentchat.channels import Channel, ThreadHistoryChannel
import agentchat.integrations
from agentchat.channels.discord.rendering import text_payload
import agentchat.channels.discord.events
import agentchat.channels.teams.events
import agentchat.channels.imessage.events
import agentchat.channels.whatsapp.events
import agentchat.channels.ycloud.events
import agentchat.channels.telegram.events
from agentchat.channels.telegram.rendering import text_payload
assert 'agentchat.channels.slack' not in sys.modules
assert 'agentchat.integrations.openai_agents' not in sys.modules
"""
    )


@pytest.mark.parametrize(
    "provider,extra",
    [
        ("Discord", "discord"),
        ("Teams", "teams"),
        ("IMessage", "imessage"),
    ],
)
def test_optional_provider_has_actionable_import_error(provider, extra):
    run_script(
        BLOCK_IMPORTS
        + f"""
try:
    from agentchat.channels.{extra} import {provider}
except ImportError as error:
    assert 'agentchat[{extra}]' in str(error), str(error)
else:
    raise AssertionError('provider import should require optional SDK')
"""
    )


def test_openai_helpers_remain_lazy_and_actionable():
    run_script(
        BLOCK_IMPORTS
        + """
try:
    from agentchat.integrations import should_reply
except ImportError as error:
    assert 'agentchat[openai]' in str(error), str(error)
else:
    raise AssertionError('integration import should require optional SDK')
"""
    )


@pytest.mark.parametrize("order", ["explicit_first", "namespace_first"])
def test_export_identity_in_both_import_orders(order):
    explicit = """
from agentchat.channels.slack.adapter import Slack as SlackDirect, SlackUser as SlackUserDirect
from agentchat.channels.discord.adapter import Discord as DiscordDirect
from agentchat.channels.teams.adapter import Teams as TeamsDirect
from agentchat.channels.imessage.adapter import IMessage as IMessageDirect
from agentchat.channels.whatsapp.adapter import WhatsApp as WhatsAppDirect
from agentchat.channels.ycloud.adapter import YCloud as YCloudDirect
from agentchat.channels.telegram.adapter import Telegram as TelegramDirect
from agentchat.integrations.openai_agents import should_reply as should_reply_direct
"""
    namespace = """
from agentchat.channels.slack import Slack, SlackUser
from agentchat.channels.discord import Discord
from agentchat.channels.teams import Teams
from agentchat.channels.imessage import IMessage
from agentchat.channels.whatsapp import WhatsApp
from agentchat.channels.ycloud import YCloud
from agentchat.channels.telegram import Telegram
from agentchat.integrations import should_reply
"""
    checks = """
assert Slack is SlackDirect and SlackUser is SlackUserDirect
assert Discord is DiscordDirect and Teams is TeamsDirect
assert IMessage is IMessageDirect and WhatsApp is WhatsAppDirect
assert Telegram is TelegramDirect and YCloud is YCloudDirect
assert should_reply is should_reply_direct
"""
    code = explicit + namespace if order == "explicit_first" else namespace + explicit
    run_script(code + checks)
