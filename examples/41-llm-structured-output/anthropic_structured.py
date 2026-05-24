from __future__ import annotations
from pydantic import BaseModel, ConfigDict
from typhon_runtime import Ok, Err, Result
import dataclasses
import json
import os
from anthropic import Anthropic
from pydantic import Field


class Address(BaseModel):
    model_config = ConfigDict(extra="forbid")
    street: str
    city: str
    country: str


class Person(BaseModel):
    model_config = ConfigDict(extra="forbid")
    name: str
    age: int = Field(ge=0, le=130)
    email: str
    address: Address | None
    skills: list[str] = []


@dataclasses.dataclass(slots=True)
class ExtractError:
    stage: str
    detail: str


SCHEMA: dict[str, object] = Person.model_json_schema()
SYSTEM: str = "Extract a structured Person object from the user's text. Respond with valid JSON that matches the schema. No commentary, no markdown fences."


def extract_person(client: Anthropic, text: str) -> Result[Person, ExtractError]:
    prompt: str = (
        f"Schema:\n{json.dumps(SCHEMA, indent=2)}\n\nText:\n{text}\n\nReturn JSON only."
    )
    try:
        resp = client.messages.create(
            model="claude-opus-4-7",
            max_tokens=1024,
            system=SYSTEM,
            messages=[{"role": "user", "content": prompt}],
        )
    except Exception as e:
        return Err(ExtractError(stage="api", detail=str(e)))
    raw: str = ""
    for block in resp.content:
        if block.type == "text":
            raw = raw + block.text
    raw = (
        raw.strip()
        .removeprefix("```json")
        .removeprefix("```")
        .removesuffix("```")
        .strip()
    )
    try:
        parsed: dict[str, object] = json.loads(raw)
    except json.JSONDecodeError as e:
        return Err(ExtractError(stage="json", detail=f"{e}: {raw[:120]}"))
    try:
        return Ok(Person.model_validate(parsed))
    except Exception as e:
        return Err(ExtractError(stage="validation", detail=str(e)))


def main() -> None:
    key: str | None = os.environ.get("ANTHROPIC_API_KEY")
    if key is None:
        print("ANTHROPIC_API_KEY not set — skipping")
        return
    client: Anthropic = Anthropic(api_key=key)
    bio: str = "Ada Lovelace, 36, contactable at ada@example.com, lives at 1 Babbage Lane, London, UK. She's strong in mathematics, analytical engines, and pioneering computer science."
    match extract_person(client, bio):
        case Ok(person):
            print(person.model_dump_json(indent=2))
        case Err(e):
            print(f"failed at {e.stage}: {e.detail}")


if __name__ == "__main__":
    main()
