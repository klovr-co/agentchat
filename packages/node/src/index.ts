import { createRequire } from "node:module";
import { AsyncLocalStorage } from "node:async_hooks";

export type Json = null | boolean | number | string | readonly Json[] | { readonly [key: string]: Json };
export interface Sender { readonly id: string; readonly displayName: string | null }
export interface Attachment { readonly id: string; readonly name: string | null; readonly mediaType: string | null; readonly size: number | null }
export interface Message {
  readonly id: string;
  readonly conversationId: string;
  readonly channel: string;
  readonly sender: Sender;
  readonly text: string;
  readonly role: "user" | "assistant";
  readonly metadata: Readonly<Record<string, Json>>;
  readonly threadId: string | null;
  readonly addressed: boolean;
  readonly attachments: readonly Attachment[];
}

export interface RichReply {
  text: string;
  blocks?: readonly Readonly<Record<string, Json>>[];
  attachments?: readonly Readonly<Record<string, Json>>[];
}
export interface UploadFile { filename: string; content: Uint8Array; title?: string }
export interface UploadedFile { readonly id: string; readonly permalink: string | null }
export interface DownloadedFile { readonly id: string; readonly name: string; readonly content: Buffer; readonly mediaType: string | null }
export interface WorkingStatus {
  update(status: string): Promise<boolean>;
  close(): Promise<void>;
}

/** What a message does while its conversation's handler is still running. */
export type Busy = "steer" | "queue" | "ask" | "reject";
export type ChoiceStyle = "default" | "primary" | "danger";
/** One button of a prompt. `id` is returned in the answer; people see `label`. */
export interface Choice { readonly id: string; readonly label: string; readonly style?: ChoiceStyle }
/** The button someone pressed, and who pressed it. */
export interface Answer { readonly prompt: string; readonly choice: string; readonly label: string; readonly sender: Sender }
/** Who may answer: by default the sender of the message the prompt is about. */
export type Allowed = "anyone" | readonly string[];

export interface SlackOptions {
  botToken: string;
  appToken: string;
  workspaceId?: string;
  botUserId?: string;
  ackEmoji?: string | null;
  maxPendingEvents?: number;
  shutdownTimeoutMs?: number;
  /** Slack Web API root. HTTP is accepted only for loopback development fixtures. */
  apiBaseUrl?: string;
}

export class Slack {
  readonly #options: SlackOptions;

  constructor(options: SlackOptions) {
    if (!options.botToken || !options.appToken) throw new Error("Slack requires botToken and appToken");
    this.#options = { ...options };
  }

  static fromEnv(options: Partial<SlackOptions> = {}): Slack {
    return new Slack({
      ...options,
      botToken: options.botToken ?? process.env.SLACK_BOT_TOKEN ?? "",
      appToken: options.appToken ?? process.env.SLACK_APP_TOKEN ?? "",
      ...(options.workspaceId ?? process.env.SLACK_TEAM_ID ? { workspaceId: options.workspaceId ?? process.env.SLACK_TEAM_ID! } : {}),
      ...(options.botUserId ?? process.env.SLACK_BOT_USER_ID ? { botUserId: options.botUserId ?? process.env.SLACK_BOT_USER_ID! } : {}),
    });
  }

  /** @internal */
  nativeOptions(): SlackOptions { return { ...this.#options }; }
}

interface WireMessage {
  id: string; conversation_id: string; channel: string;
  sender: { id: string; display_name: string | null };
  text: string; role: "user" | "assistant"; metadata?: Record<string, Json>;
  thread_id?: string | null; addressed?: boolean;
  attachments?: { id: string; name?: string | null; media_type?: string | null; size?: number | null }[];
}

function messageFromWire(wire: WireMessage): Message {
  return Object.freeze({
    id: wire.id, conversationId: wire.conversation_id, channel: wire.channel,
    sender: Object.freeze({ id: wire.sender.id, displayName: wire.sender.display_name ?? null }),
    text: wire.text, role: wire.role, metadata: Object.freeze(wire.metadata ?? {}),
    threadId: wire.thread_id ?? null, addressed: wire.addressed ?? true,
    attachments: Object.freeze((wire.attachments ?? []).map((file) => Object.freeze({
      id: file.id, name: file.name ?? null, mediaType: file.media_type ?? null, size: file.size ?? null,
    }))),
  });
}

function messagesFromJson(json: string): readonly Message[] {
  return Object.freeze((JSON.parse(json) as WireMessage[]).map(messageFromWire));
}

function messageToWire(message: Message): WireMessage {
  return {
    id: message.id, conversation_id: message.conversationId, channel: message.channel,
    sender: { id: message.sender.id, display_name: message.sender.displayName },
    text: message.text, role: message.role, metadata: { ...message.metadata },
    thread_id: message.threadId, addressed: message.addressed,
    attachments: message.attachments.map((file) => ({
      id: file.id, name: file.name, media_type: file.mediaType, size: file.size,
    })),
  };
}

interface NativeStream {
  append(text: string): Promise<void>;
  finish(): Promise<string>;
  abort(): Promise<void>;
}

interface NativeApp {
  run(handler: (channelIndex: number, messageJson: string, turnId: number) => Promise<string | null>): Promise<void>;
  close(): Promise<void>;
  reply(channelIndex: number, sourceJson: string, text: string): Promise<string>;
  history(conversationId: string, limit?: number): Promise<string>;
  threadHistory(channelIndex: number, sourceJson: string, limit: number): Promise<string>;
  subscribe(channelIndex: number, sourceJson: string): Promise<void>;
  replyRich(channelIndex: number, sourceJson: string, contentJson: string): Promise<string>;
  uploadFiles(channelIndex: number, sourceJson: string, files: { filename: string; content: Buffer; title?: string }[]): Promise<string>;
  downloadAttachment(channelIndex: number, sourceJson: string, attachmentJson: string, maxBytes: number): Promise<DownloadedFile>;
  setStatus(channelIndex: number, sourceJson: string, status: string): Promise<boolean>;
  mirror(channelIndex: number, sourceJson: string, messageJson: string, origin: string): Promise<string>;
  working(channelIndex: number, sourceJson: string, status: string, refreshIntervalMs: number): Promise<WorkingStatus>;
  steered(turnId: number): string;
  stopped(turnId: number): boolean;
  waitStopped(turnId: number): Promise<boolean>;
  stop(conversationId: string): boolean;
  ask(channelIndex: number, sourceJson: string, text: string, choicesJson: string, timeoutMs: number, allowedJson?: string): Promise<string>;
  offerStop(channelIndex: number, sourceJson: string, turnId: number, text: string, label: string, allowedJson?: string): Promise<string | null>;
  stream(channelIndex: number, sourceJson: string, intervalMs?: number): NativeStream;
}

const require = createRequire(import.meta.url);
type NativeConstructor = new (channelsJson: string, busy?: string) => NativeApp;
function loadNative(): { NativeAgentChat: NativeConstructor } {
  return require("../native/index.cjs") as { NativeAgentChat: NativeConstructor };
}

function choicesToWire(choices: readonly (Choice | string)[]): string {
  return JSON.stringify(choices.map((choice, index) => typeof choice === "string"
    ? { id: String(index + 1), label: choice, style: "default" }
    : { id: choice.id, label: choice.label, style: choice.style ?? "default" }));
}

function allowedToWire(allowed: Allowed | undefined): string | undefined {
  if (allowed === undefined) return undefined;
  if (allowed !== "anyone" && !Array.isArray(allowed)) throw new TypeError('allowed must be "anyone" or a list of sender IDs');
  return JSON.stringify(allowed);
}

/**
 * An answer shown while it is written. Each `append()` adds text; updates are sent
 * at most once per interval. `finish()` sends the rest and confirms the answer as
 * ordinary messages in history. Call `abort()` instead when the handler fails.
 */
export class AnswerStream {
  readonly #native: NativeStream;
  #text = "";
  #messages: readonly Message[] = Object.freeze([]);

  /** @internal */
  constructor(native: NativeStream) { this.#native = native; }

  /** Everything appended so far. */
  get text(): string { return this.#text; }
  /** The confirmed messages after `finish()`; long answers span several. */
  get messages(): readonly Message[] { return this.#messages; }

  async append(text: string): Promise<void> {
    await this.#native.append(text);
    this.#text += text;
  }

  /** Returns the answer's last message, or null when nothing was written. */
  async finish(): Promise<Message | null> {
    this.#messages = messagesFromJson(await this.#native.finish());
    return this.#messages.at(-1) ?? null;
  }

  /** Close a started stream with the text so far, without adding it to history. */
  async abort(): Promise<void> {
    await this.#native.abort();
  }
}

export class Conversation {
  readonly #native: NativeApp;
  constructor(readonly id: string, native: NativeApp) {
    this.#native = native;
    Object.freeze(this);
  }

  async history(options: { limit?: number } = {}): Promise<readonly Message[]> {
    if (options.limit !== undefined && (!Number.isInteger(options.limit) || options.limit < 0 || options.limit > 0xffff_ffff)) {
      throw new RangeError("History limit must be a non-negative 32-bit integer");
    }
    return messagesFromJson(await this.#native.history(this.id, options.limit));
  }
}

export class MessageContext {
  readonly message: Message;
  readonly sender: Sender;
  readonly conversation: Conversation;
  readonly #native: NativeApp;
  readonly #channelIndex: number;
  readonly #sourceJson: string;
  readonly #turnId: number;

  /** @internal */
  constructor(
    native: NativeApp,
    channelIndex: number,
    sourceJson: string,
    readonly signal: AbortSignal,
    turnId = -1,
  ) {
    this.#native = native;
    this.#channelIndex = channelIndex;
    this.#sourceJson = sourceJson;
    this.#turnId = turnId;
    this.message = messageFromWire(JSON.parse(sourceJson) as WireMessage);
    this.sender = this.message.sender;
    this.conversation = new Conversation(this.message.conversationId, native);
    Object.freeze(this);
  }

  async reply(content: string): Promise<Message> {
    return messageFromWire(JSON.parse(await this.#native.reply(this.#channelIndex, this.#sourceJson, content)) as WireMessage);
  }

  async history(options: { limit?: number } = {}): Promise<readonly Message[]> {
    const limit = options.limit ?? 30;
    if (!Number.isInteger(limit) || limit < 2 || limit > 100) throw new RangeError("History limit must be between 2 and 100");
    return messagesFromJson(await this.#native.threadHistory(this.#channelIndex, this.#sourceJson, limit));
  }

  /** Subscribe this Slack thread to future messages, including messages without a mention. */
  async subscribe(): Promise<void> {
    await this.#native.subscribe(this.#channelIndex, this.#sourceJson);
  }

  async replyRich(content: RichReply): Promise<Message> {
    return messageFromWire(JSON.parse(await this.#native.replyRich(this.#channelIndex, this.#sourceJson, JSON.stringify(content))) as WireMessage);
  }

  async uploadFiles(files: readonly UploadFile[]): Promise<readonly UploadedFile[]> {
    const uploaded = await this.#native.uploadFiles(this.#channelIndex, this.#sourceJson, files.map((file) => ({
      filename: file.filename, content: Buffer.from(file.content), ...(file.title === undefined ? {} : { title: file.title }),
    })));
    return Object.freeze(JSON.parse(uploaded) as UploadedFile[]);
  }

  async downloadAttachment(attachment: Attachment, options: { maxBytes?: number } = {}): Promise<DownloadedFile> {
    const maxBytes = options.maxBytes ?? 10 * 1024 * 1024;
    if (!Number.isSafeInteger(maxBytes) || maxBytes <= 0 || maxBytes > 0xffff_ffff) throw new RangeError("maxBytes must be a positive 32-bit integer");
    const file = await this.#native.downloadAttachment(this.#channelIndex, this.#sourceJson, JSON.stringify({
      id: attachment.id, name: attachment.name, media_type: attachment.mediaType, size: attachment.size,
    }), maxBytes);
    return Object.freeze({ ...file, mediaType: file.mediaType ?? null });
  }

  async setStatus(status: string): Promise<boolean> {
    return this.#native.setStatus(this.#channelIndex, this.#sourceJson, status);
  }

  /** Refresh a transient status. Always await close() in a finally block. */
  async working(options: { status?: string; refreshIntervalMs?: number } = {}): Promise<WorkingStatus> {
    const interval = options.refreshIntervalMs ?? 60_000;
    if (!Number.isInteger(interval) || interval <= 0 || interval > 90_000) throw new RangeError("Refresh interval must be between 1 and 90000 milliseconds");
    return this.#native.working(this.#channelIndex, this.#sourceJson, options.status ?? "is working…", interval);
  }

  /**
   * Messages sent to this conversation since the handler started or last asked.
   * Each is returned once; any left unread run as their own turns afterwards.
   */
  steered(): readonly Message[] {
    return messagesFromJson(this.#native.steered(this.#turnId));
  }

  /** True once someone asked this handler to stop, with `app.stop()` or a Stop button. */
  get stopped(): boolean {
    return this.#native.stopped(this.#turnId);
  }

  /** Resolves when someone asks this handler to stop. Stopping is cooperative. */
  async waitStopped(): Promise<void> {
    if (!(await this.#native.waitStopped(this.#turnId))) await new Promise<never>(() => {});
  }

  /**
   * Show `text` with one button per choice and wait for an allowed person to press
   * one. Throws a TimeoutError DOMException when nobody answers within `timeoutMs`
   * (default 10 minutes), so an unanswered approval is never a yes.
   */
  async ask(text: string, choices: readonly (Choice | string)[], options: { timeoutMs?: number; allowed?: Allowed } = {}): Promise<Answer> {
    const timeoutMs = options.timeoutMs ?? 600_000;
    if (!Number.isInteger(timeoutMs) || timeoutMs <= 0 || timeoutMs > 0xffff_ffff) throw new RangeError("timeoutMs must be a positive 32-bit integer");
    const result = JSON.parse(await this.#native.ask(
      this.#channelIndex, this.#sourceJson, text, choicesToWire(choices), timeoutMs, allowedToWire(options.allowed),
    )) as { answer?: { prompt: string; choice: string; label: string; sender: WireMessage["sender"] }; error?: string };
    if (result.error === "timeout") throw new DOMException("Nobody answered the prompt in time", "TimeoutError");
    if (result.error === "unsupported" || !result.answer) throw new TypeError("This channel does not support prompts");
    const { sender, ...answer } = result.answer;
    return Object.freeze({ ...answer, sender: Object.freeze({ id: sender.id, displayName: sender.display_name ?? null }) });
  }

  /** Show a Stop button for this handler; it goes away when the handler finishes. */
  async offerStop(options: { text?: string; label?: string; allowed?: Allowed } = {}): Promise<Message | null> {
    const shown = await this.#native.offerStop(
      this.#channelIndex, this.#sourceJson, this.#turnId, options.text ?? "Working on it…", options.label ?? "Stop", allowedToWire(options.allowed),
    );
    return shown === null ? null : messageFromWire(JSON.parse(shown) as WireMessage);
  }

  /** Show the answer while it is written. Return null from the handler afterwards. */
  stream(options: { intervalMs?: number } = {}): AnswerStream {
    const interval = options.intervalMs;
    if (interval !== undefined && (!Number.isInteger(interval) || interval < 0 || interval > 0xffff_ffff)) throw new RangeError("intervalMs must be a non-negative 32-bit integer");
    return new AnswerStream(this.#native.stream(this.#channelIndex, this.#sourceJson, interval));
  }

  async mirror(message: Message, options: { origin?: string } = {}): Promise<Message> {
    const mirrored = await this.#native.mirror(this.#channelIndex, this.#sourceJson, JSON.stringify(messageToWire(message)), options.origin ?? "web");
    return messageFromWire(JSON.parse(mirrored) as WireMessage);
  }
}

export type MessageHandler = (context: MessageContext) => Promise<string | null | undefined>;
export type HandlerErrorHandler = (error: unknown, context: MessageContext) => void | Promise<void>;
const handlerScope = new AsyncLocalStorage<AgentChat>();

export class AgentChat {
  readonly channels: readonly Slack[];
  readonly #native: NativeApp;
  readonly #active = new Set<AbortController>();
  readonly #shutdownTimeoutMs: number;
  readonly #onError: HandlerErrorHandler;
  #handler: MessageHandler | undefined;
  #run: Promise<void> | undefined;
  #close: Promise<void> | undefined;

  /**
   * `busy` decides what a message does while its conversation's handler runs:
   * "steer" (default) hands it to the handler through `context.steered()`, "queue"
   * runs it next, "ask" lets the sender choose with buttons, "reject" drops it.
   */
  constructor(options: { channels: readonly Slack[]; onError?: HandlerErrorHandler; busy?: Busy }) {
    if (options.channels.length === 0) throw new Error("AgentChat requires at least one channel");
    this.channels = Object.freeze([...options.channels]);
    this.#shutdownTimeoutMs = Math.max(...this.channels.map((channel) => channel.nativeOptions().shutdownTimeoutMs ?? 20_000));
    this.#onError = options.onError ?? (() => { console.error("AgentChat message handler failed"); });
    const { NativeAgentChat } = loadNative();
    this.#native = new NativeAgentChat(JSON.stringify(this.channels.map((channel) => channel.nativeOptions())), options.busy ?? "steer");
  }

  /** Ask a conversation's running handler to stop; false when none is running. */
  stop(conversationId: string): boolean {
    return this.#native.stop(conversationId);
  }

  onMessage(handler: MessageHandler): MessageHandler {
    if (this.#handler) throw new Error("AgentChat supports one message handler");
    this.#handler = handler;
    return handler;
  }

  run(): Promise<void> {
    if (!this.#handler) return Promise.reject(new Error("Register a handler with app.onMessage"));
    if (this.#close) return Promise.reject(new Error("AgentChat is closed"));
    if (this.#run) return Promise.reject(new Error("AgentChat is already running"));
    const handler = this.#handler;
    this.#run = this.#native.run(async (channelIndex, sourceJson, turnId) => {
      const controller = new AbortController();
      this.#active.add(controller);
      const context = new MessageContext(this.#native, channelIndex, sourceJson, controller.signal, turnId);
      try {
        const result = await handlerScope.run(this, () => handler(context));
        if (result !== undefined && result !== null && typeof result !== "string") {
          throw new TypeError("Message handler must return a string or no response");
        }
        return controller.signal.aborted ? null : result ?? null;
      } catch (error) {
        if (controller.signal.aborted) return null;
        try { await handlerScope.run(this, () => this.#onError(error, context)); } catch { console.error("AgentChat onError callback failed"); }
        throw error;
      } finally {
        this.#active.delete(controller);
      }
    }).finally(() => {
      for (const controller of this.#active) controller.abort();
    });
    return this.#run;
  }

  close(): Promise<void> {
    if (!this.#close) {
      const abort = () => { for (const controller of this.#active) controller.abort(); };
      const deadline = setTimeout(abort, this.#shutdownTimeoutMs);
      deadline.unref();
      this.#close = this.#native.close().finally(() => {
        clearTimeout(deadline);
        abort();
      });
      // A handler can request shutdown, but cannot await its own completion.
      if (handlerScope.getStore() === this) void this.#close.catch(() => {});
    }
    return handlerScope.getStore() === this ? Promise.resolve() : this.#close;
  }
}
