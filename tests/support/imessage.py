import asyncio

CHAT_GUID = "iMessage;-;+15551234567"


def imessage_event(**changes):
    return {
        "guid": "incoming",
        "text": "hello",
        "isFromMe": False,
        "itemType": 0,
        "associatedMessageType": 0,
        "handle": {"address": "+15551234567"},
        "chats": [{"guid": CHAT_GUID}],
        **changes,
    }


class BubblesClient:
    server_url = "http://localhost:1234"
    socket_url = "http://localhost:1234?password=fake"

    def __init__(self, receipt):
        self.receipt = receipt
        self.closed = False

    async def check_connection(self):
        pass

    async def send_text(self, *args):
        return self.receipt.get(*args)

    async def close(self):
        self.closed = True


class TypingBubblesClient(BubblesClient):
    """A client whose server shows typing only with its Private API on."""

    def __init__(self, receipt, private_api=True):
        super().__init__(receipt)
        self.private_api = private_api
        self.typed = []

    async def start_typing(self, chat_guid):
        self.typed.append(chat_guid)
        self.receipt.get(chat_guid)
        return self.private_api


class SocketClient:
    def __init__(self):
        self.stopped, self.started = asyncio.Event(), asyncio.Event()

    def on(self, event, callback):
        self.callback = callback

    async def connect(self, url):
        self.started.set()

    async def wait(self):
        await self.stopped.wait()

    async def shutdown(self):
        self.stopped.set()
