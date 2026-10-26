import assert from "node:assert/strict";
import test from "node:test";
import { AgentChat, Slack } from "../dist/index.js";
import { slackFixture, eventually } from "./slack-fixture.mjs";

function slack(fixture, options = {}) {
  return new Slack({ botToken: "xoxb-fixture", appToken: "xapp-fixture", apiBaseUrl: fixture.apiBaseUrl, ackEmoji: null, ...options });
}

async function withApp(fixture, app, body) {
  const running = app.run();
  void running.catch(() => {});
  try {
    await body();
  } finally {
    try { await app.close(); await running; } finally { await fixture.close(); }
  }
}

const interactive = (envelope, payload) => ({ type: "interactive", envelope_id: envelope, payload });
const button = (actionId, value = "plain") => ({
  type: "block_actions", team: { id: "T1" }, trigger_id: "1.2.3", user: { id: "U1", team_id: "T1" },
  channel: { id: "C1" }, message: { ts: "200.1" }, actions: [{ action_id: actionId, value, action_ts: "5.1" }],
});
const submission = (callbackId) => ({
  type: "view_submission", team: { id: "T1" }, user: { id: "U1", team_id: "T1" },
  view: { id: "V1", callback_id: callbackId },
});
const ack = (fixture, envelope) => fixture.acknowledgements.find((item) => item.envelope_id === envelope);

test("Slack handlers receive app payloads and answer in the acknowledgement", { timeout: 15000 }, async () => {
  const fixture = await slackFixture();
  const channel = slack(fixture);
  const app = new AgentChat({ channels: [channel] });
  const seen = [];
  app.onMessage(async (context) => `echo:${context.message.text}`);
  channel.onEvent("app_home_opened", async (payload) => {
    seen.push(`home:${payload.event.tab}`);
    await channel.publishHome(payload.event.user, { type: "home", blocks: [] });
  });
  channel.onAction("retry", async (payload) => {
    seen.push("retry");
    await channel.openView(payload.trigger_id, { type: "modal", callback_id: "settings", title: { type: "plain_text", text: "Settings" } });
  });
  channel.onAction(/^open_/, () => { seen.push("open"); });
  channel.onView("settings", () => ({ response_action: "errors", errors: { name: "Required" } }));
  channel.onCommand("/deploy", (payload) => ({ text: `Deploying ${payload.text}` }));
  channel.onRaw((payload) => { seen.push(`raw:${payload.type}`); });
  assert.throws(() => channel.onEvent(/x/, () => {}), /Expected a string/);
  await withApp(fixture, app, async () => {
    await fixture.envelope({ type: "events_api", envelope_id: "home", payload: {
      team_id: "T1", event: { type: "app_home_opened", user: "U1", tab: "home" } } });
    await fixture.envelope(interactive("retry", button("retry")));
    await fixture.envelope(interactive("open", button("open_artifact_2")));
    await fixture.envelope(interactive("shortcut", { type: "shortcut", team: { id: "T1" }, callback_id: "x" }));
    await fixture.envelope(interactive("submit", submission("settings")));
    await fixture.envelope({ type: "slash_commands", envelope_id: "cmd", payload: { team_id: "T1", command: "/deploy", text: "api" } });
    await fixture.send("hello");
    await eventually(() => seen.length === 4 && ack(fixture, "submit") && ack(fixture, "cmd")
      && fixture.calls.some((call) => call.method === "chat.postMessage"));
    assert.deepEqual(seen.sort(), ["home:home", "open", "raw:shortcut", "retry"]);
    assert.deepEqual(ack(fixture, "home"), { envelope_id: "home" });
    assert.deepEqual(ack(fixture, "submit").payload, { response_action: "errors", errors: { name: "Required" } });
    assert.deepEqual(ack(fixture, "cmd").payload, { text: "Deploying api" });
    const methods = fixture.calls.map((call) => call.method);
    assert.ok(methods.includes("views.publish") && methods.includes("views.open"));
    assert.equal(fixture.calls.find((call) => call.method === "views.open").body.trigger_id, "1.2.3");
  });
});

test("ephemeral notices, delete, task updates and native stop", { timeout: 15000 }, async () => {
  const fixture = await slackFixture();
  const channel = slack(fixture, { agentSessions: true });
  const app = new AgentChat({ channels: [channel] });
  let stopped;
  const stopSeen = new Promise((resolve) => { stopped = resolve; });
  app.onMessage(async (context) => {
    const notice = await context.replyEphemeral("Only you");
    assert.equal(notice.metadata.ephemeral, true);
    await assert.rejects(channel.delete(notice), /ephemeral/);
    const reply = await context.reply("Working…");
    await channel.delete(reply);
    await context.setStatus("is working…");
    const stream = context.stream({ intervalMs: 0 });
    await stream.plan("Fixing it");
    await stream.task("t1", "Read logs", "in_progress");
    await stream.append("Done.");
    await stream.task("t1", "Read logs", "complete", { output: "ok" });
    await context.waitStopped();
    stopped();
    await stream.finish();
    return null;
  });
  await withApp(fixture, app, async () => {
    await fixture.send("go");
    await eventually(() => fixture.calls.some((call) => call.method === "chat.appendStream" && call.body.chunks?.[0]?.status === "complete"));
    await fixture.envelope({ type: "events_api", envelope_id: "stop", payload: { team_id: "T1", event: {
      type: "agent_session_stopped", channel: "C1", thread_ts: "100.1", user: "U1", event_ts: "300.9", streaming_message_ts: [] } } });
    await stopSeen;
    await eventually(() => fixture.calls.some((call) => call.method === "chat.stopStream"));
    const ephemeral = fixture.calls.find((call) => call.method === "chat.postEphemeral").body;
    assert.deepEqual([ephemeral.channel, ephemeral.user, ephemeral.thread_ts], ["C1", "U1", "100.1"]);
    assert.equal(fixture.calls.find((call) => call.method === "chat.delete").body.channel, "C1");
    const start = fixture.calls.find((call) => call.method === "chat.startStream").body;
    assert.equal(start.task_display_mode, "plan");
    assert.deepEqual(start.chunks, [{ type: "plan_update", title: "Fixing it" }]);
    const statuses = fixture.calls.filter((call) => call.method === "agents.sessions.setStatus").map((call) => call.body.status);
    assert.deepEqual(statuses.slice(0, 2), ["processing", "active"]);
  });
});

test("Slack operations need an AgentChat", () => {
  const channel = new Slack({ botToken: "xoxb-x", appToken: "xapp-x" });
  return assert.rejects(channel.openView("1.2.3", { type: "modal" }), /Add this Slack channel/);
});
