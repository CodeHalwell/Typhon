from __future__ import annotations
import dataclasses
import os
import sys
import time
from anthropic import Anthropic


@dataclasses.dataclass(slots=True)
class StreamStats:
    output_tokens: int
    elapsed_s: float
    chars: int


def get_client() -> Anthropic | None:
    key: str | None = os.environ.get("ANTHROPIC_API_KEY")
    if key is None:
        return None
    return Anthropic(api_key=key)


def stream_completion(client: Anthropic, prompt: str) -> StreamStats:
    chars: int = 0
    tokens: int = 0
    start: float = time.monotonic()
    with client.messages.stream(
        model="claude-opus-4-7",
        max_tokens=1024,
        system="You are a poetic technical writer.",
        messages=[{"role": "user", "content": prompt}],
    ) as stream:
        for text in stream.text_stream:
            sys.stdout.write(text)
            sys.stdout.flush()
            chars = chars + len(text)
        final = stream.get_final_message()
        tokens = int(final.usage.output_tokens)
    sys.stdout.write("""
""")
    return StreamStats(
        output_tokens=tokens, elapsed_s=time.monotonic() - start, chars=chars
    )


def main() -> None:
    client: Anthropic | None = get_client()
    if client is None:
        print("ANTHROPIC_API_KEY not set — skipping")
        return
    stats: StreamStats = stream_completion(
        client, prompt="Write a four-line ode to a well-typed compiler."
    )
    print(
        f"\n[{stats.chars} chars, {stats.output_tokens} tokens, {stats.elapsed_s:.2f}s]"
    )


if __name__ == "__main__":
    main()
