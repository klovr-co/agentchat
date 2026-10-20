import base64
import hashlib
import hmac
import json
import time

from aiohttp import web

BUSINESS = "+15550001111"
CUSTOMER = "+15551234567"
SECRET = "whsec_secret"
API_KEY = "key"


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


def status_event(status, *, event_id="evt_status", **fields):
    """`whatsapp.message.updated`, shaped like YCloud's documented examples."""
    message = {
        "id": "ycloud-out",
        "wamid": "wamid.sent",
        "to": CUSTOMER,
        "status": status,
        "externalId": "outbox-1",
        "createTime": "2026-10-21T12:00:00.000Z",
        "sendTime": "2026-10-21T12:00:01.000Z",
        "type": "text",
        "text": {"body": "private reply text"},
    }
    if status in ("delivered", "read"):
        message["deliverTime"] = "2026-10-21T12:00:02.000Z"
    if status == "read":
        message["readTime"] = "2026-10-21T12:00:03.000Z"
    message.update(fields)
    return {
        "id": event_id,
        "type": "whatsapp.message.updated",
        "apiVersion": "v2",
        "createTime": "2026-10-21T12:00:05.000Z",
        "whatsappMessage": message,
    }


def echo_event(**fields):
    """`whatsapp.smb.message.echoes`: staff replied from the WhatsApp Business app."""
    message = {
        "id": "ycloud-echo",
        "wamid": "wamid.echo",
        "wabaId": "waba",
        "from": BUSINESS,
        "to": CUSTOMER,
        "type": "text",
        "text": {"body": "staff typed this in the app"},
        "status": "sent",
    }
    message.update(fields)
    return {
        "id": "evt_echo",
        "type": "whatsapp.smb.message.echoes",
        "apiVersion": "v2",
        "createTime": "2026-10-21T12:00:00.000Z",
        "whatsappMessage": message,
    }


def signed(payload, timestamp=None, secret=SECRET):
    raw = json.dumps(payload).encode()
    timestamp = int(time.time()) if timestamp is None else timestamp
    signature = hmac.new(secret.encode(), f"{timestamp}.".encode() + raw, hashlib.sha256)
    return {
        "data": raw,
        "headers": {"YCloud-Signature": f"t={timestamp},s={signature.hexdigest()}"},
    }


def template(name, language="en", status="APPROVED", category="UTILITY"):
    return {
        "wabaId": "waba",
        "name": name,
        "language": language,
        "status": status,
        "category": category,
        "components": [
            {"type": "HEADER", "format": "TEXT", "text": "Update"},
            {"type": "BODY", "text": f"{name} body {{{{1}}}}"},
        ],
    }


class FakeYCloud:
    """Simulated YCloud API: direct send, template listing, and media download.

    Follows the documented shapes; it cannot prove the live service behaves the same.
    """

    def __init__(self):
        self.sends = []
        self.send_responses = []
        self.template_requests = []
        self.templates = []
        self.media = {}
        self.media_requests = []
        self.link_fresh = True

    def app(self):
        app = web.Application()
        app.router.add_post("/v2/whatsapp/messages/sendDirectly", self._send)
        app.router.add_get("/v2/whatsapp/templates", self._templates)
        app.router.add_get("/v2/whatsapp/media/download/{id}", self._download)
        return app

    def fail_next_send(self, status, code=None, whatsapp_code=None):
        error = {"status": status, "code": code or "BAD_REQUEST", "requestId": "req_1"}
        if whatsapp_code:
            error["whatsappApiError"] = {"code": whatsapp_code, "message": "m", "type": "t"}
        self.send_responses.append((status, {"error": error}))

    async def _send(self, request):
        assert request.headers["X-API-Key"] == API_KEY
        body = await request.json()
        self.sends.append(body)
        if self.send_responses:
            status, payload = self.send_responses.pop(0)
            return web.json_response(payload, status=status)
        number = len(self.sends)
        return web.json_response(
            {
                "id": f"ycloud-out-{number}",
                "wamid": f"wamid.sent-{number}",
                "status": "accepted",
                "externalId": body.get("externalId"),
            }
        )

    async def _templates(self, request):
        assert request.headers["X-API-Key"] == API_KEY
        query = dict(request.query)
        self.template_requests.append(query)
        statuses = query.get("filter.status")
        items = [
            item
            for item in self.templates
            if (not statuses or item["status"] in statuses.split(","))
            and ("filter.language" not in query or item["language"] == query["filter.language"])
        ]
        page, limit = int(query.get("page", 1)), int(query.get("limit", 10))
        chunk = items[(page - 1) * limit : page * limit]
        return web.json_response(
            {"offset": (page - 1) * limit, "limit": limit, "length": len(chunk), "items": chunk}
        )

    def add_media(self, media_id, content, mime_type="image/jpeg", chunked=False):
        self.media[media_id] = (content, mime_type, chunked)
        digest = base64.b64encode(hashlib.sha256(content).digest()).decode()
        return digest

    async def _download(self, request):
        media_id = request.match_info["id"]
        with_key = request.headers.get("X-API-Key")
        self.media_requests.append({"id": media_id, "with_key": with_key is not None,
                                    "sig": request.query.get("sig")})
        if with_key is not None and with_key != API_KEY:
            return web.Response(status=401)
        if with_key is None and not (self.link_fresh and request.query.get("sig")):
            return web.Response(status=401)
        if media_id not in self.media:
            return web.Response(status=404)
        content, mime_type, chunked = self.media[media_id]
        if not chunked:
            return web.Response(body=content, content_type=mime_type)
        response = web.StreamResponse(headers={"Content-Type": mime_type})
        response.enable_chunked_encoding()
        await response.prepare(request)
        for start in range(0, len(content), 4096):
            await response.write(content[start : start + 4096])
        await response.write_eof()
        return response


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

    async def read(self):
        return json.dumps(await self.json()).encode()

    async def close(self):
        self.closed = True
