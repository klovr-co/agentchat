import assert from "node:assert/strict";
import test from "node:test";
import { AgentChat, Slack } from "../dist/index.js";
import { slackFixture, eventually } from "./slack-fixture.mjs";

function setup(fixture, options = {}) {
  return new AgentChat({ ...options, channels: [new Slack({ botToken: "xoxb-fixture", appToken: "xapp-fixture", apiBaseUrl: fixture.apiBaseUrl, ackEmoji: null })] });
}

function gate() {
  let open;
  const promise = new Promise((resolve) => { open = resolve; });
  return { promise, open };
}

const posts = (fixture) => fixture.calls.filter((call) => call.method === "chat.postMessage");
const buttons = (post) => post.body.blocks.at(-1).elements;

async function withApp(fixture, app, body) {
  const running = app.run();
  void running.catch(() => {});
  try {
    await body();
  } finally {
    try { await app.close(); await running; } finally { await fixture.close(); }
  }
}

test("steer is the default: a message sent while the handler runs reaches steered()", { timeout: 10000 }, async () => {
  const fixture = await slackFixture();
  const app = setup(fixture);
  const release = gate();
  const started = gate();
  const seen = [];
  app.onMessage(async (context) => {
    seen.push(context.message.text);
    if (context.message.text !== "first") return null;
    started.open();
    await release.promise;
    return `steered:${context.steered().map((message) => message.text).join(",")}`;
  });
  await withApp(fixture, app, async () => {
    await fixture.send("first");
    await started.promise;
    await fixture.send("second", { ts: "100.2" });
    await eventually(() => fixture.acknowledgements.length === 2);
    await new Promise((resolve) => setTimeout(resolve, 50));
    release.open();
    await eventually(() => posts(fixture).length === 1);
    assert.equal(posts(fixture)[0].body.text, "steered:second");
    assert.deepEqual(seen, ["first"]);
  });
});

test("busy: queue runs the second message as its own turn", { timeout: 10000 }, async () => {
  const fixture = await slackFixture();
  const app = setup(fixture, { busy: "queue" });
  const release = gate();
  const started = gate();
  app.onMessage(async (context) => {
    if (context.message.text === "first") {
      started.open();
      await release.promise;
    }
    assert.deepEqual(context.steered(), []);
    return `answer:${context.message.text}`;
  });
  await withApp(fixture, app, async () => {
    await fixture.send("first");
    await started.promise;
    await fixture.send("second", { ts: "100.2" });
    await eventually(() => fixture.acknowledgements.length === 2);
    release.open();
    await eventually(() => posts(fixture).length === 2);
    assert.deepEqual(posts(fixture).map((call) => call.body.text), ["answer:first", "answer:second"]);
  });
  assert.throws(() => setup(fixture, { busy: "later" }), /busy must be/);
});

test("ask shows Block Kit buttons, accepts only the sender, and closes the prompt", { timeout: 10000 }, async () => {
  const fixture = await slackFixture();
  const app = setup(fixture);
  let answer;
  app.onMessage(async (context) => {
    answer = await context.ask("Deploy?", [{ id: "yes", label: "Deploy", style: "primary" }, "Cancel"]);
    return `chose:${answer.choice}`;
  });
  await withApp(fixture, app, async () => {
    await fixture.send("deploy");
    await eventually(() => posts(fixture).length === 1);
    const prompt = posts(fixture)[0];
    assert.equal(prompt.body.thread_ts, "100.1");
    const [deploy, cancel] = buttons(prompt);
    assert.equal(deploy.style, "primary");
    assert.match(cancel.value, /^ac:[0-9a-f]{16}:2$/);
    await fixture.press(deploy.value, { user: "U2" });
    await new Promise((resolve) => setTimeout(resolve, 50));
    assert.equal(answer, undefined);
    await fixture.press(deploy.value, { user: "U1" });
    await eventually(() => posts(fixture).length === 2);
    assert.equal(posts(fixture)[1].body.text, "chose:yes");
    assert.deepEqual({ ...answer, sender: { ...answer.sender } }, { prompt: deploy.value.split(":")[1], choice: "yes", label: "Deploy", sender: { id: "U1", displayName: "u1" } });
    const update = fixture.calls.find((call) => call.method === "chat.update");
    assert.match(update.body.ts, /^200\./);
    assert.equal(update.body.text, "Deploy?\n\n✓ Deploy — u1");
  });
});

test("an unanswered prompt fails closed with a TimeoutError", { timeout: 10000 }, async () => {
  const fixture = await slackFixture();
  const app = setup(fixture);
  let failure;
  app.onMessage(async (context) => {
    try {
      await context.ask("Delete?", ["Yes"], { timeoutMs: 100 });
    } catch (error) {
      failure = error;
    }
    return null;
  });
  await withApp(fixture, app, async () => {
    await fixture.send("delete");
    await eventually(() => failure !== undefined);
    assert.equal(failure.name, "TimeoutError");
    await eventually(() => fixture.calls.some((call) => call.method === "chat.update"));
    await assert.rejects(Promise.reject(failure), (error) => error instanceof DOMException);
  });
});

test("a Stop button and app.stop() stop the running handler cooperatively", { timeout: 10000 }, async () => {
  const fixture = await slackFixture();
  const app = setup(fixture);
  assert.equal(app.stop("slack:T1:C1:100.1"), false);
  const offered = [];
  app.onMessage(async (context) => {
    assert.equal(context.stopped, false);
    offered.push(await context.offerStop({ label: "Halt" }));
    await context.waitStopped();
    assert.equal(context.stopped, true);
    return `stopped:${context.message.text}`;
  });
  await withApp(fixture, app, async () => {
    await fixture.send("button");
    await eventually(() => posts(fixture).length === 1);
    const [stop] = buttons(posts(fixture)[0]);
    assert.equal(stop.style, "danger");
    assert.equal(stop.text.text, "Halt");
    await fixture.press(stop.value);
    await eventually(() => posts(fixture).length === 2);
    assert.equal(posts(fixture)[1].body.text, "stopped:button");

    await fixture.send("api", { ts: "100.3", thread: "100.3" });
    await eventually(() => posts(fixture).length === 3);
    assert.equal(app.stop("slack:T1:C1:100.3"), true);
    await eventually(() => posts(fixture).length === 4);
    assert.equal(posts(fixture)[3].body.text, "stopped:api");
    // The unanswered Stop button went away with its turn.
    await eventually(() => fixture.calls.filter((call) => call.method === "chat.update").length === 2);
    assert.equal(offered.length, 2);
    assert(offered.every((message) => message.role === "assistant"));
  });
});

test("stream() uses Slack's native stream in threads and confirms the answer", { timeout: 10000 }, async () => {
  const fixture = await slackFixture();
  const app = setup(fixture);
  let streamed;
  app.onMessage(async (context) => {
    const stream = context.stream({ intervalMs: 0 });
    await stream.append("Hello");
    await stream.append(", world");
    streamed = await stream.finish();
    assert.equal(stream.text, "Hello, world");
    assert.equal(stream.messages.length, 1);
    const history = await context.conversation.history();
    assert.equal(history.at(-1).text, "Hello, world");
    return null;
  });
  await withApp(fixture, app, async () => {
    await fixture.send("stream");
    await eventually(() => streamed !== undefined);
    const methods = fixture.calls.map((call) => call.method).filter((method) => method.includes("Stream"));
    assert.deepEqual(methods, ["chat.startStream", "chat.appendStream", "chat.stopStream"]);
    const start = fixture.calls.find((call) => call.method === "chat.startStream").body;
    assert.equal(start.thread_ts, "100.1");
    assert.equal(start.recipient_user_id, "U1");
    assert.equal(fixture.calls.find((call) => call.method === "chat.appendStream").body.markdown_text, ", world");
    assert.equal(streamed.text, "Hello, world");
    assert.equal(posts(fixture).length, 0);
  });
});
