from __future__ import annotations
from pydantic import BaseModel, ConfigDict
import dataclasses
from datetime import datetime


class AskRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    question: str


class AskResponse(BaseModel):
    model_config = ConfigDict(extra="forbid")
    answer: str
    notes_saved: int
    tool_calls: int


class NoteOut(BaseModel):
    model_config = ConfigDict(extra="forbid")
    id: int
    title: str
    body: str
    created_at: str


@dataclasses.dataclass(slots=True)
class Note:
    id: int
    title: str
    body: str
    created_at: datetime

    def to_out(self) -> NoteOut:
        return NoteOut(
            id=self.id,
            title=self.title,
            body=self.body,
            created_at=self.created_at.isoformat(),
        )


type AgentEvent = ThoughtEvent | ToolCallEvent | FinalAnswerEvent


@dataclasses.dataclass(slots=True)
class ThoughtEvent:
    text: str


@dataclasses.dataclass(slots=True)
class ToolCallEvent:
    name: str
    args: dict[str, object]


@dataclasses.dataclass(slots=True)
class FinalAnswerEvent:
    text: str
