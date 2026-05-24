from __future__ import annotations
import dataclasses
import re


@dataclasses.dataclass(slots=True)
class LogLine:
    timestamp: str
    level: str
    message: str


LOG_PATTERN: re.Pattern[str] = re.compile(
    "^\\[(?P<ts>\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2})\\]\\s+(?P<level>[A-Z]+)\\s+(?P<msg>.+)$"
)


def parse_log_line(line: str) -> LogLine | None:
    m: re.Match[str] | None = LOG_PATTERN.match(line)
    if m is None:
        return None
    return LogLine(
        timestamp=m.group("ts"), level=m.group("level"), message=m.group("msg")
    )


def extract_emails(text: str) -> list[str]:
    return re.findall("[\\w._%+-]+@[\\w.-]+\\.[A-Za-z]{2,}", text)


def redact_credit_cards(text: str) -> str:
    return re.sub(
        "\\b\\d{4}[ -]?\\d{4}[ -]?\\d{4}[ -]?\\d{4}\\b", "XXXX-XXXX-XXXX-XXXX", text
    )


def split_camel_case(s: str) -> list[str]:
    return re.findall("[A-Z][a-z]*|[a-z]+", s)


def main() -> None:
    lines: list[str] = [
        "[2026-05-18T12:00:01] INFO server started on port 8080",
        "[2026-05-18T12:00:05] WARN slow query (1.2s)",
        "this line will not match",
        "[2026-05-18T12:00:09] ERROR connection refused to db",
    ]
    for raw in lines:
        parsed: LogLine | None = parse_log_line(raw)
        if parsed is None:
            print(f"skip: {raw}")
            continue
        print(f"{parsed.level:5s} {parsed.timestamp} -> {parsed.message}")
    print(extract_emails("contact ada@example.com or grace@navy.mil for help"))
    print(redact_credit_cards("card 4111 1111 1111 1111 expires soon"))
    print(split_camel_case("HTTPSConnectionFactoryBuilder"))


if __name__ == "__main__":
    main()
