# 47 — Mini research assistant

A small multi-file project that ties most of the earlier examples together:

- **`src/models.ty`** — `model` types at the HTTP boundary, internal `class` types,
  sealed-union tool events.
- **`src/store.ty`** — SQLite-backed note storage with `Result[T, E]` returns.
- **`src/agent.ty`** — Anthropic-backed agent loop with two tools (`save_note`,
  `search_notes`) and a sealed-union event stream.
- **`src/api.ty`** — FastAPI endpoints: ask a question, list saved notes.
- **`src/main.ty`** — uvicorn entry point.

## Run

```bash
export ANTHROPIC_API_KEY=sk-ant-...
tyc build examples/47-mini-app/src/
python build/main.py
# in another shell:
curl -X POST localhost:8000/ask -H 'content-type: application/json' \
     -d '{"question":"What is the Pythagorean theorem? Save a one-liner note."}'
curl localhost:8000/notes
```
