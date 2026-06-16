from __future__ import annotations


def parse_query(query: str) -> dict[str, str]:
    params: dict[str, str] = {}
    for pair in query.split("&"):
        if "=" in pair:
            parts: list[str] = pair.split("=", 1)
            params[parts[0]] = parts[1]
    return params


def parse_csv_line(line: str) -> list[str]:
    return [field.strip() for field in line.split(",")]


def main() -> None:
    print(parse_query("name=alice&age=30&city=nyc"))
    print(parse_csv_line("  a , b ,  c  , d "))


if __name__ == "__main__":
    main()
