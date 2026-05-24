from __future__ import annotations
from typhon_runtime import Ok, Err, Result
import dataclasses
import os
from anthropic import Anthropic


@dataclasses.dataclass(slots=True)
class LlmError:
    kind: str
    message: str


def get_client() -> Result[Anthropic, LlmError]:
    key: str | None = os.environ.get("ANTHROPIC_API_KEY")
    if key is None:
        return Err(LlmError(kind="config", message="set ANTHROPIC_API_KEY"))
    return Ok(Anthropic(api_key=key))


def ask(
    client: Anthropic, prompt: str, system: str = "You are concise."
) -> Result[str, LlmError]:
    try:
        resp = client.messages.create(
            model="claude-opus-4-7",
            max_tokens=1024,
            system=system,
            messages=[{"role": "user", "content": prompt}],
        )
    except Exception as e:
        return Err(LlmError(kind="api", message=str(e)))
    parts: list[str] = []
    for block in resp.content:
        if block.type == "text":
            parts.append(block.text)
    return Ok("".join(parts))


def summarise(client: Anthropic, text: str) -> Result[str, LlmError]:
    prompt: str = f"Summarise the following passage in one sentence:\n\n{text}"
    return ask(client, prompt, system="You write concise summaries.")


def classify_sentiment(client: Anthropic, text: str) -> Result[str, LlmError]:
    prompt: str = f"Classify the sentiment of this text as POSITIVE, NEGATIVE, or NEUTRAL. Reply with only the single word.\n\nText: {text}"
    return ask(client, prompt, system="You are a sentiment classifier.")


def run(client: Anthropic) -> None:
    passage: str = "Typhon is a statically-typed superset of Python that compiles to clean Python source. It catches null-safety bugs, requires let/mut on locals, and emits ordinary .py files with no runtime dependency."
    match summarise(client, passage):
        case Ok(summary):
            print(f"summary: {summary}")
        case Err(e):
            print(f"err: {e.kind}/{e.message}")
    for review in ["Loved it!", "Terrible, would not return.", "It's okay."]:
        match classify_sentiment(client, review):
            case Ok(label):
                print(f"  {label.strip():10s} <- {review}")
            case Err(e):
                print(f"  err: {e.message}")


def main() -> None:
    match get_client():
        case Ok(client):
            run(client)
        case Err(e):
            print(f"skip: {e.message}")


if __name__ == "__main__":
    main()
