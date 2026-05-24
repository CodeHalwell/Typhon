from __future__ import annotations
import dataclasses
from typhon_runtime.lazy import lazy_import as __typhon_lazy_import

torch = __typhon_lazy_import("torch")
from transformers import AutoModelForSequenceClassification, AutoTokenizer, pipeline


@dataclasses.dataclass(slots=True)
class SentimentResult:
    text: str
    label: str
    score: float


def run_pipeline(sentences: list[str]) -> list[SentimentResult]:
    clf = pipeline(
        "sentiment-analysis", model="distilbert-base-uncased-finetuned-sst-2-english"
    )
    raw: list[dict[str, object]] = clf(sentences)
    return [
        SentimentResult(text=text, label=str(r["label"]), score=float(r["score"]))
        for (text, r) in zip(sentences, raw)
    ]


def run_manual(sentences: list[str]) -> list[SentimentResult]:
    model_id: str = "distilbert-base-uncased-finetuned-sst-2-english"
    tokenizer = AutoTokenizer.from_pretrained(model_id)
    model = AutoModelForSequenceClassification.from_pretrained(model_id)
    model.eval()
    inputs = tokenizer(sentences, return_tensors="pt", padding=True, truncation=True)
    with torch.no_grad():
        logits: torch.Tensor = model(**inputs).logits
        probs: torch.Tensor = torch.softmax(logits, dim=-1)
        pred_ids: torch.Tensor = probs.argmax(dim=-1)
    results: list[SentimentResult] = []
    for i, text in enumerate(sentences):
        label_id: int = int(pred_ids[i].item())
        label: str = model.config.id2label[label_id]
        score: float = float(probs[i, label_id].item())
        results.append(SentimentResult(text=text, label=label, score=score))
    return results


def main() -> None:
    samples: list[str] = [
        "This compiler is delightful — finally, types that bite.",
        "Production downtime cost us a small fortune today.",
        "It works, I guess. Could be worse.",
    ]
    print("--- pipeline ---")
    for pipe_r in run_pipeline(samples):
        print(f"  {pipe_r.label:10s} {pipe_r.score:.3f}  {pipe_r.text}")
    print("--- manual ---")
    for manual_r in run_manual(samples):
        print(f"  {manual_r.label:10s} {manual_r.score:.3f}  {manual_r.text}")


if __name__ == "__main__":
    main()
