# agentchat

AgentChat has a shared Rust core with compatible Python, Node.js/TypeScript and
Rust interfaces. The native Node/Rust channel currently targets Slack, including
the [Claude Agent SDK example](examples/node/README.md). Existing Python channel
integrations remain supported through native bindings. Implementation code lives under
`src/`; `python/agentchat/` contains only the compatible interface. See the [migration and support guide](docs/rust-core.md).

a minimal sdk to connect your agents to slack, teams, imessage, whatsapp, ycloud, telegram.

use any SDK (Claude Agents, OpenAI Agents, Langchain DeepAgents, Pydantic Agents) and instantly connect it to all supported channels.

<img width="3840" height="2160" alt="agentchat-cinematic-4k" src="https://github.com/user-attachments/assets/bccdba03-80e6-4c11-83c5-d150d1263189" />

## Quickstart

### 1. Install

Installing from this checkout requires a Rust toolchain. Published binary wheels
do not require a compiler.
```sh
git clone https://github.com/BerriAI/agentchat.git
cd agentchat
python -m pip install -e ".[openai]"
```
### 2. Usage
Save this as `bot.py`:

```python
import asyncio
import os

from agents import Agent, Runner, SQLiteSession # import `Agent` from OpenAI Agents SDK

from agentchat import AgentChat, MessageContext
from agentchat.channels.slack import Slack

app = AgentChat(channels=[Slack.from_env(ack_emoji=None)])

agent = Agent(name="Assistant", model=os.environ["OPENAI_MODEL"])
sessions = {}


@app.on_message
async def respond(context: MessageContext) -> str:
    key = context.conversation.id
    if key not in sessions:
        sessions[key] = SQLiteSession(key)

    result = await Runner.run(
        agent,
        input=context.message.text,
        session=sessions[key],
    )
    return str(result.final_output)


async def main():
    try:
        await app.run()
    finally:
        await app.close()


asyncio.run(main())
```


## Other agent SDKs

### 1. Claude Agent SDK

```python
from claude_agent_sdk import ClaudeAgentOptions, ResultMessage, query

sessions = {}


@app.on_message
async def respond(context: MessageContext) -> str:
    key = context.conversation.id
    options = ClaudeAgentOptions(resume=sessions.get(key), tools=[])

    result = None
    async for event in query(prompt=context.message.text, options=options):
        if isinstance(event, ResultMessage):
            sessions[key] = event.session_id
            result = event

    if result is None or result.is_error or not result.result:
        raise RuntimeError("Claude did not return a successful answer")
    return result.result
```

The mapping connects each chat conversation to its Claude session.
[Claude session documentation](https://code.claude.com/docs/en/agent-sdk/sessions)

### 2. Langchain Deep Agents

```python
from deepagents import create_deep_agent
from langgraph.checkpoint.memory import InMemorySaver

agent = create_deep_agent(
    model=os.environ["DEEP_AGENT_MODEL"],
    checkpointer=InMemorySaver(),
)


@app.on_message
async def respond(context: MessageContext) -> str:
    result = await agent.ainvoke(
        {"messages": [{"role": "user", "content": context.message.text}]},
        config={"configurable": {"thread_id": context.conversation.id}},
    )
    if result.get("__interrupt__"):
        raise RuntimeError("This example does not handle approval requests")
    answer = result["messages"][-1]
    if answer.type != "ai" or answer.tool_calls or not answer.text:
        raise RuntimeError("The agent did not return a final text answer")
    return answer.text
```

The checkpointer keeps a separate conversation for each thread ID. Deep Agents
retains its standard planning, virtual filesystem, and subagent tools.
[Deep Agents quickstart](https://docs.langchain.com/oss/python/deepagents/quickstart)

### 3. Pydantic AI

Install `pydantic-ai-slim[openai]`. Reuse `OPENAI_API_KEY` and `OPENAI_MODEL`
from the quickstart.

```python
from pydantic_ai import Agent
from pydantic_ai.messages import ModelMessage

agent = Agent(f"openai:{os.environ['OPENAI_MODEL']}", output_type=str)
histories: dict[str, list[ModelMessage]] = {}


@app.on_message
async def respond(context: MessageContext) -> str:
    key = context.conversation.id
    result = await agent.run(
        context.message.text,
        message_history=histories.get(key, []),
    )
    histories[key] = result.all_messages()
    return result.output
```

`all_messages()` includes the previous history and the current run. Replace the
stored history with this list after each turn.

### 4. LangGraph SDK

```python
from langgraph_sdk import get_client

client = get_client(
    url=os.environ["LANGGRAPH_URL"],
    api_key=os.getenv("LANGSMITH_API_KEY"),
)
threads: dict[str, str] = {}


@app.on_message
async def respond(context: MessageContext) -> str:
    key = context.conversation.id
    if key not in threads:
        thread = await client.threads.create()
        threads[key] = thread["thread_id"]

    result = await client.runs.wait(
        threads[key],
        os.environ["LANGGRAPH_ASSISTANT_ID"],
        input={"messages": [{"role": "user", "content": context.message.text}]},
    )
    if result.get("__interrupt__"):
        raise RuntimeError("The graph is waiting for approval")
    answer = result["messages"][-1]
    if answer["type"] != "ai" or answer.get("tool_calls"):
        raise RuntimeError("The graph did not return a final answer")
    content = answer["content"]
    if not isinstance(content, str) or not content:
        raise RuntimeError("This example expects a text answer")
    return content
```

## Live status, streaming, buttons, and busy conversations

`context.working()` shows that the agent is busy, `context.stream()` shows the
answer while it is written, `context.ask()` asks for an approval with buttons,
and `context.offer_stop()` adds a Stop button. A message that arrives while its
conversation's handler is running is steered into it by default
(`context.steered()`); messages the handler leaves unread run afterwards. Earlier
versions made such messages wait for the next turn; pass
`AgentChat(busy="queue")` to keep that. See [Live controls](docs/live-controls.md).
Streams can show task and plan progress next to the answer (`reply.task()`,
`reply.plan()`) where the channel supports it.

## Slack app features

Every Slack payload reaches your app through AgentChat, so one app token serves
both the agent and the rest of the app: `@slack.on_event("app_home_opened")`,
`@slack.on_action("retry")`, `@slack.on_view("settings")` (its return value is
the acknowledgement, such as `response_action`), `@slack.on_command("/deploy")`
and `@slack.on_raw`. Slack also gets private notices (`context.reply_ephemeral()`),
modals and App Home (`slack.open_view()`, `push_view()`, `update_view()`,
`publish_home()`), `app.delete()`, and Slack's native stop button. See
[Slack app features](docs/slack.md#slack-app-features) and
[Moving from Slack Bolt](docs/slack.md#moving-from-slack-bolt).

Telegram, Discord, Teams, WhatsApp, YCloud and iMessage take the same handlers
and use their own equivalents: Telegram ephemeral messages and draft stop
buttons, Discord interaction responses and modals, Teams targeted messages,
dialogs and stream Stop, WhatsApp Flows, and fallbacks where a provider has
none. See [App features](docs/app-features.md).

## Supported channels

| Channel | Connection | Install extra | Setup |
| --- | --- | --- | --- |
| Slack | Socket Mode | Included | [Slack guide](docs/slack.md) |
| Microsoft Teams | Authenticated HTTPS endpoint | `teams` | [Teams guide](docs/teams.md) |
| iMessage | BlueBubbles on a Mac signed into Messages | `imessage` | [iMessage guide](docs/imessage.md) |
| WhatsApp | Business Cloud API with signed webhooks | `whatsapp` | [WhatsApp guide](docs/whatsapp.md) |
| YCloud (WhatsApp) | YCloud WhatsApp API with signed webhooks | `ycloud` | [YCloud guide](docs/ycloud.md) |
| Discord | Bot gateway | `discord` | [Discord guide](docs/discord.md) |
| Telegram | Bot API long polling or HTTPS webhook | `telegram` | [Telegram guide](docs/telegram.md) |
