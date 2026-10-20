BUSINESS = "+15550001111"
CUSTOMER = "+15551234567"


def ycloud_message(**fields):
    message = {
        "id": "ycloud-in",
        "wamid": "wamid.incoming",
        "wabaId": "waba",
        "from": CUSTOMER,
        "customerProfile": {"name": "Alice"},
        "to": BUSINESS,
        "type": "text",
        "text": {"body": "hello"},
    }
    message.update(fields)
    return message


def ycloud_event(message=None):
    return {
        "id": "evt_1",
        "type": "whatsapp.inbound_message.received",
        "apiVersion": "v2",
        "createTime": "2026-10-21T12:00:00.000Z",
        "whatsappInboundMessage": message if message is not None else ycloud_message(),
    }


class YCloudSession:
    def __init__(self, receipt):
        self.receipt, self.closed = receipt, False
        self.status = 200

    def post(self, *args, **kwargs):
        self.value = self.receipt.get(*args, **kwargs)
        return self

    async def __aenter__(self):
        return self

    async def __aexit__(self, *args):
        pass

    async def json(self):
        if self.value is None:
            return {"status": "failed"}
        return {"id": "ycloud-out", "wamid": self.value, "status": "sent"}

    async def close(self):
        self.closed = True
