from __future__ import annotations
import dataclasses
import asyncio
import os
from anthropic import AsyncAnthropic

type Task = ResearchTask | SummariseTask | CritiqueTask


@dataclasses.dataclass(slots=True)
class ResearchTask:
    topic: str


@dataclasses.dataclass(slots=True)
class SummariseTask:
    text: str


@dataclasses.dataclass(slots=True)
class CritiqueTask:
    draft: str


@dataclasses.dataclass(slots=True)
class AgentReply:
    role: str
    text: str


@dataclasses.dataclass(slots=True)
class Blackboard:
    notes: dict[str, str]

    def post(self, key: str, value: str) -> None:
        self.notes[key] = value

    def get(self, key: str) -> str | None:
        return self.notes.get(key)


async def call_claude(client: AsyncAnthropic, system: str, prompt: str) -> str:
    resp = await client.messages.create(
        model="claude-opus-4-7",
        max_tokens=1024,
        system=system,
        messages=[{"role": "user", "content": prompt}],
    )
    parts: list[str] = []
    for block in resp.content:
        if block.type == "text":
            parts.append(block.text)
    return "".join(parts)


async def researcher(client: AsyncAnthropic, topic: str) -> AgentReply:
    prompt: str = f"Provide 3 concise factual bullets about: {topic}"
    text: str = await call_claude(client, "You are a careful researcher.", prompt)
    return AgentReply(role="researcher", text=text)


async def summariser(client: AsyncAnthropic, text: str) -> AgentReply:
    prompt: str = f"Summarise into a single tweet-length sentence:\n\n{text}"
    out: str = await call_claude(client, "You produce tight summaries.", prompt)
    return AgentReply(role="summariser", text=out)


async def critic(client: AsyncAnthropic, draft: str) -> AgentReply:
    prompt: str = f"Critique this draft. Be specific and brief:\n\n{draft}"
    out: str = await call_claude(client, "You are a rigorous editor.", prompt)
    return AgentReply(role="critic", text=out)


async def router(client: AsyncAnthropic, task: Task) -> AgentReply:
    match task:
        case ResearchTask(topic):
            return await researcher(client, topic)
        case SummariseTask(text):
            return await summariser(client, text)
        case CritiqueTask(draft):
            return await critic(client, draft)


async def pipeline(client: AsyncAnthropic, topic: str, board: Blackboard) -> None:
    research: AgentReply = await router(client, ResearchTask(topic=topic))
    board.post("research", research.text)
    print(f"\n[researcher]\n{research.text}")
    async with asyncio.TaskGroup() as __typhon_tg_0__:
        __typhon_gather_1__ = __typhon_tg_0__.create_task(
            router(client, SummariseTask(text=research.text))
        )
        __typhon_gather_2__ = __typhon_tg_0__.create_task(
            router(client, CritiqueTask(draft=research.text))
        )
    summary = __typhon_gather_1__.result()
    critique = __typhon_gather_2__.result()
    board.post("summary", summary.text)
    board.post("critique", critique.text)
    print(f"\n[summariser]\n{summary.text}")
    print(f"\n[critic]\n{critique.text}")


async def main_async() -> None:
    key: str | None = os.environ.get("ANTHROPIC_API_KEY")
    if key is None:
        print("ANTHROPIC_API_KEY not set — skipping")
        return
    client: AsyncAnthropic = AsyncAnthropic(api_key=key)
    board: Blackboard = Blackboard(notes={})
    await pipeline(
        client,
        topic="The Pythagorean theorem and one surprising application.",
        board=board,
    )
    print(f"\n[blackboard keys] {list(board.notes.keys())}")


def main() -> None:
    asyncio.run(main_async())


if __name__ == "__main__":
    main()
