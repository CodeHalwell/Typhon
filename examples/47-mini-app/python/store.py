from __future__ import annotations
from typhon_runtime import Ok, Err, Result
import dataclasses
import sqlite3
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path
from typing import Iterator
from models import Note


@dataclasses.dataclass(slots=True)
class StoreError:
    op: str
    reason: str


@dataclasses.dataclass(slots=True)
class NoteStore:
    db_path: Path

    @contextmanager
    def _connect(self) -> Iterator[sqlite3.Connection]:
        conn: sqlite3.Connection = sqlite3.connect(str(self.db_path))
        try:
            yield conn
            conn.commit()
        finally:
            conn.close()

    def init_schema(self) -> None:
        with self._connect() as conn:
            conn.executescript("""
                CREATE TABLE IF NOT EXISTS notes (
                    id          INTEGER PRIMARY KEY AUTOINCREMENT,
                    title       TEXT NOT NULL,
                    body        TEXT NOT NULL,
                    created_at  TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_notes_title ON notes(title);
            """)

    def save(self, title: str, body: str) -> Result[Note, StoreError]:
        now: datetime = datetime.now(timezone.utc)
        try:
            with self._connect() as conn:
                cur = conn.execute(
                    "INSERT INTO notes (title, body, created_at) VALUES (?, ?, ?)",
                    (title, body, now.isoformat()),
                )
                new_id: int = int(cur.lastrowid or 0)
            return Ok(Note(id=new_id, title=title, body=body, created_at=now))
        except sqlite3.Error as e:
            return Err(StoreError(op="save", reason=str(e)))

    def search(self, needle: str, limit: int = 10) -> list[Note]:
        rows: list[Note] = []
        with self._connect() as conn:
            cur = conn.execute(
                "SELECT id, title, body, created_at FROM notes WHERE title LIKE ? OR body LIKE ? ORDER BY id DESC LIMIT ?",
                (f"%{needle}%", f"%{needle}%", limit),
            )
            rows = [
                Note(
                    id=int(row[0]),
                    title=str(row[1]),
                    body=str(row[2]),
                    created_at=datetime.fromisoformat(str(row[3])),
                )
                for row in cur.fetchall()
            ]
        return rows

    def list_recent(self, limit: int = 20) -> list[Note]:
        rows: list[Note] = []
        with self._connect() as conn:
            cur = conn.execute(
                "SELECT id, title, body, created_at FROM notes ORDER BY id DESC LIMIT ?",
                (limit,),
            )
            rows = [
                Note(
                    id=int(row[0]),
                    title=str(row[1]),
                    body=str(row[2]),
                    created_at=datetime.fromisoformat(str(row[3])),
                )
                for row in cur.fetchall()
            ]
        return rows


def open_store(path: Path) -> NoteStore:
    store: NoteStore = NoteStore(db_path=path)
    store.init_schema()
    return store
