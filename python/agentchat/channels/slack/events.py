"""Slack event conversion implemented in Rust."""
import re

from agentchat._native import SLACK_ACK_DEADLINE as ACK_DEADLINE
from agentchat._native import slack_attachments as slack_attachments
from agentchat._native import slack_filename as _filename
from agentchat._native import slack_responds as responds
from agentchat._native import slack_to_message as to_message

FILE_ID = re.compile(r"F[A-Z0-9]+")

__all__ = ['ACK_DEADLINE', 'FILE_ID', '_filename', 'responds', 'slack_attachments', 'to_message']
