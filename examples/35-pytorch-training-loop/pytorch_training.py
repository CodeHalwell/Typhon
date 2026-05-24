from __future__ import annotations
import dataclasses
from typhon_runtime.lazy import lazy_import as __typhon_lazy_import

torch = __typhon_lazy_import("torch")
nn = __typhon_lazy_import("torch.nn")
from torch.utils.data import DataLoader, TensorDataset


@dataclasses.dataclass(slots=True)
class ToyClassifier:
    net: nn.Sequential

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        return self.net(x)


def make_classifier(in_dim: int, n_classes: int) -> ToyClassifier:
    net: nn.Sequential = nn.Sequential(
        nn.Linear(in_dim, 64),
        nn.ReLU(),
        nn.Linear(64, 64),
        nn.ReLU(),
        nn.Linear(64, n_classes),
    )
    return ToyClassifier(net=net)


@dataclasses.dataclass(slots=True)
class EpochStats:
    epoch: int
    train_loss: float
    val_loss: float
    val_acc: float


def make_dataloaders(seed: int = 0) -> tuple[DataLoader, DataLoader]:
    g: torch.Generator = torch.Generator().manual_seed(seed)
    x: torch.Tensor = torch.randn(1000, 8, generator=g)
    y: torch.Tensor = (x.sum(dim=1) > 0.0).long()
    split: int = 800
    train_ds: TensorDataset = TensorDataset(x[:split], y[:split])
    val_ds: TensorDataset = TensorDataset(x[split:], y[split:])
    train_loader: DataLoader = DataLoader(train_ds, batch_size=64, shuffle=True)
    val_loader: DataLoader = DataLoader(val_ds, batch_size=64)
    return (train_loader, val_loader)


def train_one_epoch(
    model: ToyClassifier,
    loader: DataLoader,
    optimiser: torch.optim.Optimizer,
    criterion: nn.Module,
    device: torch.device,
) -> float:
    model.net.train()
    total_loss: float = 0.0
    n_samples: int = 0
    for xb, yb in loader:
        x_dev: torch.Tensor = xb.to(device)
        y_dev: torch.Tensor = yb.to(device)
        optimiser.zero_grad()
        logits: torch.Tensor = model.forward(x_dev)
        loss: torch.Tensor = criterion(logits, y_dev)
        loss.backward()
        optimiser.step()
        total_loss = total_loss + loss.item() * float(x_dev.size(0))
        n_samples = n_samples + int(x_dev.size(0))
    return total_loss / float(n_samples)


def evaluate(
    model: ToyClassifier, loader: DataLoader, criterion: nn.Module, device: torch.device
) -> tuple[float, float]:
    model.net.eval()
    total_loss: float = 0.0
    correct: int = 0
    n: int = 0
    with torch.no_grad():
        for xb, yb in loader:
            x_dev: torch.Tensor = xb.to(device)
            y_dev: torch.Tensor = yb.to(device)
            logits: torch.Tensor = model.forward(x_dev)
            total_loss = total_loss + criterion(logits, y_dev).item() * float(
                x_dev.size(0)
            )
            preds: torch.Tensor = logits.argmax(dim=1)
            correct = correct + int((preds == y_dev).sum().item())
            n = n + int(x_dev.size(0))
    return (total_loss / float(n), float(correct) / float(n))


def train(epochs: int = 5) -> list[EpochStats]:
    device: torch.device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
    (train_loader, val_loader) = make_dataloaders()
    model: ToyClassifier = make_classifier(in_dim=8, n_classes=2)
    model.net.to(device)
    optimiser: torch.optim.Optimizer = torch.optim.Adam(
        model.net.parameters(), lr=0.001
    )
    criterion: nn.Module = nn.CrossEntropyLoss()
    history: list[EpochStats] = []
    e: int = 1
    while e <= epochs:
        train_loss: float = train_one_epoch(
            model, train_loader, optimiser, criterion, device
        )
        (val_loss, val_acc) = evaluate(model, val_loader, criterion, device)
        history.append(
            EpochStats(
                epoch=e, train_loss=train_loss, val_loss=val_loss, val_acc=val_acc
            )
        )
        print(f"epoch {e}: train={train_loss:.4f} val={val_loss:.4f} acc={val_acc:.3f}")
        e = e + 1
    return history


def main() -> None:
    train(epochs=5)


if __name__ == "__main__":
    main()
