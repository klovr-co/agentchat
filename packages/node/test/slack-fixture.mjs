import http from "node:http";
import { once } from "node:events";
import { WebSocketServer } from "ws";

export async function slackFixture({ replyDelayMs = 0, statusClearDelayMs = 0, statusStartGate } = {}) {
  const calls = [];
  const sockets = new Set();
  const acknowledgements = [];
  const server = http.createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const text = Buffer.concat(chunks).toString();
    const body = request.headers["content-type"]?.includes("application/json")
      ? JSON.parse(text || "{}") : Object.fromEntries(new URLSearchParams(text));
    const method = new URL(request.url, "http://localhost").pathname.split("/").at(-1);
    calls.push({ method, body, bytes: Buffer.concat(chunks), authorization: request.headers.authorization });
    if (request.url.startsWith("/files-pri/")) {
      response.writeHead(200, { "content-type": "application/octet-stream" });
      response.end("hello");
      return;
    }
    if (request.url.startsWith("/upload/")) {
      response.writeHead(200);
      response.end("ok");
      return;
    }
    let result;
    if (method === "auth.test") result = { ok: true, team_id: "T1", user_id: "B1", bot_id: "BOT1" };
    else if (method === "apps.connections.open") result = { ok: true, url: `ws://127.0.0.1:${server.address().port}/socket` };
    else if (method === "chat.postMessage") result = { ok: true, channel: body.channel, ts: `200.${calls.length}`, message: { text: body.text, user: "B1" } };
    else if (method === "conversations.replies") result = { ok: true, has_more: false, messages: [{ type: "message", ts: "100.1", user: "U1", text: "root" }] };
    else if (method === "assistant.threads.setStatus") result = { ok: true };
    else if (method === "chat.startStream") result = { ok: true, channel: body.channel, ts: `300.${calls.length}` };
    else if (method === "chat.appendStream" || method === "chat.stopStream") result = { ok: true, channel: body.channel, ts: body.ts };
    else if (method === "chat.update") result = { ok: true, channel: body.channel, ts: body.ts };
    else if (method === "reactions.add") result = { ok: true };
    else if (method === "chat.postEphemeral") result = { ok: true, message_ts: `400.${calls.length}` };
    else if (method === "chat.delete") result = { ok: true, channel: body.channel, ts: body.ts };
    else if (method === "agents.sessions.setStatus") result = { ok: true, status: body.status };
    else if (method.startsWith("views.")) result = { ok: true, view: { id: "V1", ...body.view } };
    else if (method === "chat.getPermalink") result = { ok: true, permalink: "https://slack.example/message" };
    else if (method === "files.info") result = { ok: true, file: { id: "F1", name: "hello.txt", size: 5, mimetype: "text/plain", url_private_download: `http://127.0.0.1:${server.address().port}/files-pri/F1` } };
    else if (method === "files.getUploadURLExternal") result = { ok: true, file_id: "F2", upload_url: `http://127.0.0.1:${server.address().port}/upload/F2` };
    else if (method === "files.completeUploadExternal") result = { ok: true, files: [{ id: "F2", permalink: "https://slack.example/file" }] };
    else result = { ok: false, error: `unsupported_fixture_method:${method}` };
    if (method === "chat.postMessage" && replyDelayMs > 0) {
      await new Promise((resolve) => setTimeout(resolve, replyDelayMs));
      if (response.destroyed) return;
    }
    if (method === "assistant.threads.setStatus") {
      if (body.status && statusStartGate) await statusStartGate;
      if (!body.status && statusClearDelayMs > 0) await new Promise((resolve) => setTimeout(resolve, statusClearDelayMs));
      if (response.destroyed) return;
    }
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify(result));
  });
  const websocket = new WebSocketServer({ server });
  let connected;
  const connection = new Promise((resolve) => { connected = resolve; });
  websocket.on("connection", (socket) => {
    sockets.add(socket);
    socket.on("close", () => sockets.delete(socket));
    socket.on("message", (data) => acknowledgements.push(JSON.parse(data.toString())));
    socket.send(JSON.stringify({ type: "hello" }));
    connected(socket);
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  return {
    apiBaseUrl: `http://127.0.0.1:${server.address().port}/api/`,
    calls, acknowledgements, connection,
    async send(text, { ts = "100.1", thread = "100.1", channel = "C1", envelope = `env-${ts}`, type = "app_mention", files = [] } = {}) {
      const socket = await connection;
      socket.send(JSON.stringify({
        type: "events_api", envelope_id: envelope,
        payload: { team_id: "T1", event_id: `Ev-${ts}`, event: {
          type, user: "U1", channel, channel_type: "channel", text, ts, thread_ts: thread, files,
        } },
      }));
    },
    /** Press a button on a message the bot posted in thread 100.1 of C1. */
    async press(value, { user = "U1", ts = "250.1", messageTs = "200.1", envelope = `press-${ts}-${user}` } = {}) {
      const socket = await connection;
      socket.send(JSON.stringify({
        type: "interactive", envelope_id: envelope,
        payload: {
          type: "block_actions", team: { id: "T1" }, user: { id: user, name: user.toLowerCase() },
          channel: { id: "C1" }, message: { ts: messageTs, thread_ts: "100.1" },
          actions: [{ action_id: "agentchat", value, action_ts: ts }],
        },
      }));
    },
    /** Send any Socket Mode envelope. */
    async envelope(envelope) {
      const socket = await connection;
      socket.send(JSON.stringify(envelope));
    },
    async close() {
      for (const socket of sockets) socket.terminate();
      await new Promise((resolve) => websocket.close(resolve));
      server.closeAllConnections();
      await new Promise((resolve) => server.close(resolve));
    },
  };
}

export async function eventually(predicate, timeout = 4000) {
  const deadline = Date.now() + timeout;
  while (!(await predicate())) {
    if (Date.now() > deadline) throw new Error("Timed out waiting for fixture condition");
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}
