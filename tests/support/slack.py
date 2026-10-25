from agentchat import Message, Sender


class FakeSlackClient:
    def __init__(self) -> None:
        self.posts: list[dict[str, object]] = []
        self.reactions: list[dict[str, object]] = []
        self.api_calls: list[tuple[str, dict[str, object]]] = []

    async def chat_postMessage(self, **kwargs):
        self.posts.append(kwargs)
        return {"ok": True, "ts": str(len(self.posts))}

    async def reactions_add(self, **kwargs):
        self.reactions.append(kwargs)

    async def api_call(self, api_method, *, json=None, **kwargs):
        self.api_calls.append((api_method, json))
        return {"ok": True, "ts": "5.0"} if api_method == "chat.startStream" else {"ok": True}


def channel_event(ts="1.0", text="<@BOT> inspect <@TARGET>", **extra):
    return {
        "event_id": f"event-{ts}",
        "team_id": "team-1",
        "event": {
            "type": "app_mention",
            "channel": "channel-1",
            "user": "user-1",
            "ts": ts,
            "text": text,
            **extra,
        },
    }


def slack_source(thread='1.0'):
    return Message(id='slack:event', conversation_id='slack:T:C:1.0', channel='slack',
                   sender=Sender('U'), text='Start', role='user', thread_id=thread,
                   metadata={'team_id': 'T', 'channel_id': 'C',
                             'reply_thread_timestamp': thread, 'message_timestamp': '9.0'})
