from __future__ import annotations
import dataclasses
import sqlite3
from pathlib import Path


@dataclasses.dataclass(slots=True)
class Book:
    id: int
    title: str
    author: str
    year: int
    rating: float | None


def open_db(path: Path) -> sqlite3.Connection:
    conn = sqlite3.connect(str(path))
    conn.row_factory = sqlite3.Row
    return conn


def init_schema(conn: sqlite3.Connection) -> None:
    conn.executescript("""
        CREATE TABLE IF NOT EXISTS books (
            id     INTEGER PRIMARY KEY AUTOINCREMENT,
            title  TEXT NOT NULL,
            author TEXT NOT NULL,
            year   INTEGER NOT NULL,
            rating REAL
        );
    """)
    conn.commit()


def insert_book(
    conn: sqlite3.Connection, title: str, author: str, year: int, rating: float | None
) -> int:
    cur = conn.execute(
        "INSERT INTO books (title, author, year, rating) VALUES (?, ?, ?, ?)",
        (title, author, year, rating),
    )
    conn.commit()
    return int(cur.lastrowid or 0)


def find_by_author(conn: sqlite3.Connection, author: str) -> list[Book]:
    cur = conn.execute(
        "SELECT id, title, author, year, rating FROM books WHERE author = ? ORDER BY year",
        (author,),
    )
    return [
        Book(
            id=row["id"],
            title=row["title"],
            author=row["author"],
            year=row["year"],
            rating=row["rating"],
        )
        for row in cur.fetchall()
    ]


def average_rating(conn: sqlite3.Connection) -> float | None:
    cur = conn.execute("SELECT AVG(rating) AS avg FROM books WHERE rating IS NOT NULL")
    row = cur.fetchone()
    if row is None or row["avg"] is None:
        return None
    return float(row["avg"])


def transactional_bulk_insert(
    conn: sqlite3.Connection, rows: list[tuple[str, str, int, float | None]]
) -> int:
    try:
        with conn:
            conn.executemany(
                "INSERT INTO books (title, author, year, rating) VALUES (?, ?, ?, ?)",
                rows,
            )
        return len(rows)
    except sqlite3.Error as e:
        print(f"bulk insert failed, rolled back: {e}")
        return 0


def main() -> None:
    path: Path = Path("/tmp/typhon-books.db")
    if path.exists():
        path.unlink()
    conn = open_db(path)
    init_schema(conn)
    insert_book(conn, "The Mythical Man-Month", "F. Brooks", 1975, 4.5)
    bulk: list[tuple[str, str, int, float | None]] = []
    r1: float | None = 4.6
    r2: float | None = 3.9
    r3: float | None = 4.4
    r4: float | None = None
    bulk.append(("Code Complete", "S. McConnell", 1993, r1))
    bulk.append(("Clean Code", "R. Martin", 2008, r2))
    bulk.append(("Refactoring", "M. Fowler", 1999, r3))
    bulk.append(("The C Programming Language", "K&R", 1978, r4))
    transactional_bulk_insert(conn, bulk)
    for b in find_by_author(conn, "M. Fowler"):
        print(f"{b.title} ({b.year}) — {b.rating}")
    print(f"avg rating: {average_rating(conn)}")
    conn.close()


if __name__ == "__main__":
    main()
