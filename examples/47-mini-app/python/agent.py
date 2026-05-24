from __future__ import annotations
from typhon_runtime import Ok, Err, Result
import dataclasses
import json
from anthropic import Anthropic
from models import AgentEvent, FinalAnswerEvent, ThoughtEvent, ToolCallEvent
from store import NoteStore


@dataclasses.dataclass(slots=True)
class AgentError:
    stage: str
    message: str


@dataclasses.dataclass(slots=True)
class AgentRun:
    events: list[AgentEvent]
    answer: str
    tool_calls: int
    notes_saved: int


TOOLS: list[dict[str, object]] = [
    {
        "name": "save_note",
        "description": "Save a short note for later recall.",
        "input_schema": {
            "type": "object",
            "properties": {"title": {"type": "string"}, "body": {"type": "string"}},
            "required": ["title", "body"],
        },
    },
    {
        "name": "search_notes",
        "description": "Search previously saved notes by keyword.",
        "input_schema": {
            "type": "object",
            "properties": {"query": {"type": "string"}},
            "required": ["query"],
        },
    },
]
SYSTEM_PROMPT: str = "You are a research assistant. When the user asks a question, answer it concisely. If the answer is worth keeping, use save_note. If the user asks about something you may have seen before, try search_notes first."


@dataclasses.dataclass(slots=True)
class Agent:
    client: Anthropic
    store: NoteStore

    def dispatch(self, name: str, args: dict[str, object]) -> tuple[str, bool]:
        if name == "save_note":
            match self.store.save(str(args["title"]), str(args["body"])):
                case Ok(note):
                    return (json.dumps({"saved_id": note.id}), True)
                case Err(e):
                    return (json.dumps({"error": e.reason}), False)
        if name == "search_notes":
            hits = self.store.search(str(args["query"]))
            return (
                json.dumps(
                    {
                        "hits": [
                            {"id": n.id, "title": n.title, "body": n.body[:200]}
                            for n in hits
                        ]
                    }
                ),
                False,
            )
        return (json.dumps({"error": f"unknown tool {name}"}), False)

    def run(self, question: str, max_turns: int = 6) -> Result[AgentRun, AgentError]:
        messages: list[dict[str, object]] = [{"role": "user", "content": question}]
        events: list[AgentEvent] = []
        tool_calls: int = 0
        saved: int = 0
        turn: int = 0
        while turn < max_turns:
            try:
                resp = self.client.messages.create(
                    model="claude-opus-4-7",
                    max_tokens=1024,
                    system=SYSTEM_PROMPT,
                    tools=TOOLS,
                    messages=messages,
                )
            except Exception as e:
                return Err(AgentError(stage="api", message=str(e)))
            messages.append({"role": "assistant", "content": resp.content})
            if resp.stop_reason != "tool_use":
                text_parts: list[str] = []
                for text_block in resp.content:
                    if text_block.type == "text":
                        text_parts.append(text_block.text)
                final_text: str = "".join(text_parts)
                events.append(FinalAnswerEvent(text=final_text))
                return Ok(
                    AgentRun(
                        events=events,
                        answer=final_text,
                        tool_calls=tool_calls,
                        notes_saved=saved,
                    )
                )
            tool_results: list[dict[str, object]] = []
            for content_block in resp.content:
                if content_block.type == "text":
                    events.append(ThoughtEvent(text=content_block.text))
                if content_block.type == "tool_use":
                    args: dict[str, object] = dict(content_block.input)
                    events.append(ToolCallEvent(name=content_block.name, args=args))
                    tool_calls = tool_calls + 1
                    (result_text, was_save) = self.dispatch(content_block.name, args)
                    if was_save:
                        saved = saved + 1
                    tool_results.append(
                        {
                            "type": "tool_result",
                            "tool_use_id": content_block.id,
                            "content": result_text,
                        }
                    )
            messages.append({"role": "user", "content": tool_results})
            turn = turn + 1
        return Err(AgentError(stage="loop", message=f"max_turns {max_turns} exhausted"))
