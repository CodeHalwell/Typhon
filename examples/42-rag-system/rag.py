from __future__ import annotations
import dataclasses
from typhon_runtime.lazy import lazy_import as __typhon_lazy_import
import os

np = __typhon_lazy_import("numpy")
from anthropic import Anthropic
from sentence_transformers import SentenceTransformer


@dataclasses.dataclass(slots=True)
class Document:
    id: int
    text: str
    embedding: np.ndarray


@dataclasses.dataclass(slots=True)
class Hit:
    doc: Document
    score: float


@dataclasses.dataclass(slots=True)
class VectorStore:
    docs: list[Document]
    embed_dim: int

    def add(self, text: str, embedder: SentenceTransformer) -> None:
        vec: np.ndarray = embedder.encode(text, normalize_embeddings=True)
        self.docs.append(Document(id=len(self.docs), text=text, embedding=vec))

    def search(
        self, query: str, embedder: SentenceTransformer, k: int = 3
    ) -> list[Hit]:
        q: np.ndarray = embedder.encode(query, normalize_embeddings=True)
        scored: list[Hit] = [
            Hit(doc=d, score=float(np.dot(q, d.embedding))) for d in self.docs
        ]
        return sorted(scored, key=lambda h: h.score, reverse=True)[:k]


CORPUS: list[str] = [
    "Typhon is a statically-typed superset of Python. It compiles to clean CPython 3.13.",
    "Typhon uses let/mut for local bindings; module-level bindings default to let.",
    "Result[T, E] models recoverable failures. The ? operator short-circuits Err.",
    "gather: lowers parallel awaits into asyncio.TaskGroup with cancel-on-failure.",
    "Sealed unions are checked exhaustive at compile time via match statements.",
    "lazy import name = module defers module loading until first attribute access.",
    "comptime let inlines build-time constants from env vars and pure expressions.",
    "The tyc binary handles check, build, fmt, lsp, migrate, and repl subcommands.",
]


def build_store(embedder: SentenceTransformer) -> VectorStore:
    store: VectorStore = VectorStore(
        docs=[], embed_dim=embedder.get_sentence_embedding_dimension()
    )
    for text in CORPUS:
        store.add(text, embedder)
    return store


def ground(client: Anthropic, question: str, hits: list[Hit]) -> str:
    context_lines: list[str] = []
    for i, h in enumerate(hits):
        context_lines.append(f"[{i + 1}] {h.doc.text}")
    context: str = """
""".join(context_lines)
    prompt: str = f"Use only the context to answer. If the context is insufficient, say so.\n\nContext:\n{context}\n\nQuestion: {question}\nCite passages by their [n] index."
    resp = client.messages.create(
        model="claude-opus-4-7",
        max_tokens=512,
        system="You answer using only the provided context, with citations.",
        messages=[{"role": "user", "content": prompt}],
    )
    parts: list[str] = []
    for block in resp.content:
        if block.type == "text":
            parts.append(block.text)
    return "".join(parts)


def main() -> None:
    embedder: SentenceTransformer = SentenceTransformer("all-MiniLM-L6-v2")
    store: VectorStore = build_store(embedder)
    questions: list[str] = [
        "How does Typhon handle parallel awaits?",
        "What does the ? operator do?",
        "Does Typhon emit Python or have its own runtime?",
    ]
    key: str | None = os.environ.get("ANTHROPIC_API_KEY")
    client: Anthropic | None = Anthropic(api_key=key) if key is not None else None
    for q in questions:
        print(f"\nQ: {q}")
        hits: list[Hit] = store.search(q, embedder, k=3)
        for h in hits:
            print(f"  [{h.score:.3f}] {h.doc.text}")
        if client is not None:
            print(f"  answer: {ground(client, q, hits)}")


if __name__ == "__main__":
    main()
