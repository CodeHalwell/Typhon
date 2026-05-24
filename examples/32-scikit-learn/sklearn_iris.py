from __future__ import annotations
import dataclasses
from typhon_runtime.lazy import lazy_import as __typhon_lazy_import

np = __typhon_lazy_import("numpy")
from sklearn.datasets import load_iris
from sklearn.ensemble import RandomForestClassifier
from sklearn.metrics import accuracy_score, classification_report, confusion_matrix
from sklearn.model_selection import train_test_split
from sklearn.pipeline import Pipeline
from sklearn.preprocessing import StandardScaler


@dataclasses.dataclass(slots=True)
class TrainResult:
    accuracy: float
    report: str
    confusion: list[list[int]]


def build_pipeline() -> Pipeline:
    return Pipeline(
        [
            ("scaler", StandardScaler()),
            (
                "clf",
                RandomForestClassifier(n_estimators=200, max_depth=6, random_state=42),
            ),
        ]
    )


def train_and_eval(seed: int = 42) -> TrainResult:
    dataset = load_iris()
    x = dataset.data
    y = dataset.target
    (x_train, x_test, y_train, y_test) = train_test_split(
        x, y, test_size=0.25, stratify=y, random_state=seed
    )
    pipe: Pipeline = build_pipeline()
    pipe.fit(x_train, y_train)
    preds = pipe.predict(x_test)
    acc: float = float(accuracy_score(y_test, preds))
    report: str = classification_report(
        y_test, preds, target_names=list(dataset.target_names)
    )
    cm = confusion_matrix(y_test, preds)
    return TrainResult(accuracy=acc, report=report, confusion=cm.tolist())


def predict_one(features: list[float]) -> int:
    pipe: Pipeline = build_pipeline()
    dataset = load_iris()
    pipe.fit(dataset.data, dataset.target)
    return int(pipe.predict(np.array([features]))[0])


def main() -> None:
    result: TrainResult = train_and_eval()
    print(f"accuracy: {result.accuracy:.3f}")
    print(result.report)
    print("confusion matrix:")
    for row in result.confusion:
        print(f"  {row}")
    sample: list[float] = [5.1, 3.5, 1.4, 0.2]
    species: int = predict_one(sample)
    print(f"predicted class for {sample}: {species}")


if __name__ == "__main__":
    main()
