from __future__ import annotations
from pydantic import BaseModel, ConfigDict
from typhon_runtime import Ok, Err, Result
import json
from pathlib import Path


class Address(BaseModel):
    model_config = ConfigDict(extra="forbid")
    street: str
    city: str
    country: str


class Person(BaseModel):
    model_config = ConfigDict(extra="forbid")
    name: str
    age: int
    email: str
    address: Address | None
    tags: list[str] = []


def write_json(path: Path, data: dict[str, object]) -> Result[None, str]:
    try:
        path.write_text(json.dumps(data, indent=2), encoding="utf-8")
        return Ok(None)
    except OSError as e:
        return Err(f"write failed: {e}")


def load_people(path: Path) -> Result[list[Person], str]:
    try:
        raw: str = path.read_text(encoding="utf-8")
        parsed: list[dict[str, object]] = json.loads(raw)
        return Ok([Person.model_validate(p) for p in parsed])
    except FileNotFoundError:
        return Err(f"missing: {path}")
    except json.JSONDecodeError as e:
        return Err(f"invalid json: {e}")
    except Exception as e:
        return Err(f"validation error: {e}")


def adults(people: list[Person]) -> list[Person]:
    return [p for p in people if p.age >= 18]


def by_country(people: list[Person]) -> dict[str, list[str]]:
    grouped: dict[str, list[str]] = {}
    for p in people:
        country: str = p.address.country if p.address is not None else "unknown"
        if country not in grouped:
            grouped[country] = []
        grouped[country].append(p.name)
    return grouped


def main() -> None:
    path: Path = Path("/tmp/typhon-people.json")
    sample: list[dict[str, object]] = [
        {
            "name": "Ada Lovelace",
            "age": 36,
            "email": "ada@example.com",
            "address": {"street": "1 Babbage Ln", "city": "London", "country": "UK"},
            "tags": ["pioneer", "mathematician"],
        },
        {
            "name": "Linus Torvalds",
            "age": 55,
            "email": "linus@kernel.org",
            "address": {"street": "2 Penguin Rd", "city": "Portland", "country": "US"},
            "tags": ["kernel"],
        },
        {"name": "Kid Genius", "age": 12, "email": "kid@example.com", "address": None},
    ]
    write_json(path, {"people": sample})
    payload: Path = Path("/tmp/typhon-people-array.json")
    payload.write_text(json.dumps(sample), encoding="utf-8")
    match load_people(payload):
        case Ok(people):
            print(f"loaded {len(people)} people")
            for adult in adults(people):
                print(f"  adult: {adult.name} ({adult.age})")
            print(by_country(people))
        case Err(msg):
            print(f"error: {msg}")


if __name__ == "__main__":
    main()
