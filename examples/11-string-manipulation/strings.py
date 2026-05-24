from __future__ import annotations


def __typhon_ext_str__slug(self: str) -> str:
    return self.lower().strip().replace(" ", "-")


def __typhon_ext_str__truncate(self: str, n: int, ellipsis: str = "...") -> str:
    if len(self) <= n:
        return self
    return self[: n - len(ellipsis)] + ellipsis


def word_count(text: str) -> dict[str, int]:
    counts: dict[str, int] = {}
    for w in text.lower().split():
        cleaned: str = w.strip(".,!?;:()[]\"'")
        if len(cleaned) == 0:
            continue
        counts[cleaned] = counts.get(cleaned, 0) + 1
    return counts


def is_palindrome(s: str) -> bool:
    cleaned: str = "".join((c for c in s.lower() if c.isalnum()))
    return cleaned == cleaned[::-1]


def kv_parse(line: str) -> dict[str, str]:
    result: dict[str, str] = {}
    for part in line.split(","):
        chunk: str = part.strip()
        if "=" not in chunk:
            continue
        pair: list[str] = chunk.split("=", 1)
        result[pair[0].strip()] = pair[1].strip()
    return result


def render_table(headers: list[str], rows: list[list[str]]) -> str:
    widths: list[int] = [
        max(len(headers[i]), max((len(r[i]) for r in rows)))
        for i in range(len(headers))
    ]
    header: str = " | ".join((headers[i].ljust(widths[i]) for i in range(len(headers))))
    sep: str = "-+-".join(("-" * w for w in widths))
    body: list[str] = [
        " | ".join((row[i].ljust(widths[i]) for i in range(len(row)))) for row in rows
    ]
    return """
""".join([header, sep] + body)


def main() -> None:
    title: str = "  Hello, Beautiful World  "
    print(__typhon_ext_str__slug(title))
    print(__typhon_ext_str__truncate(title, 15))
    print(word_count("the cat sat on the mat, and the cat purred."))
    print(is_palindrome("A man, a plan, a canal: Panama"))
    print(kv_parse("name=Ada, role=engineer, team=core"))
    print(
        render_table(
            ["name", "score"], [["Ada", "92"], ["Grace", "88"], ["Linus", "75"]]
        )
    )


if __name__ == "__main__":
    main()
