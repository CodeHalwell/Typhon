from __future__ import annotations
import dataclasses
from typhon_runtime.lazy import lazy_import as __typhon_lazy_import

torch = __typhon_lazy_import("torch")
nn = __typhon_lazy_import("torch.nn")
F = __typhon_lazy_import("torch.nn.functional")


@dataclasses.dataclass(slots=True)
class MLP:
    layers: nn.Sequential

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        return self.layers(x)

    def parameters(self) -> object:
        return self.layers.parameters()


def make_mlp(in_dim: int, hidden: int, out_dim: int, dropout: float = 0.2) -> MLP:
    layers: nn.Sequential = nn.Sequential(
        nn.Linear(in_dim, hidden),
        nn.ReLU(),
        nn.Dropout(dropout),
        nn.Linear(hidden, hidden),
        nn.ReLU(),
        nn.Linear(hidden, out_dim),
    )
    return MLP(layers=layers)


@dataclasses.dataclass(slots=True)
class SimpleCNN:
    features: nn.Sequential
    head: nn.Linear

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        feats: torch.Tensor = self.features(x)
        return self.head(feats.flatten(1))


def make_cnn(n_classes: int = 10) -> SimpleCNN:
    features: nn.Sequential = nn.Sequential(
        nn.Conv2d(1, 16, kernel_size=3, padding=1),
        nn.ReLU(),
        nn.MaxPool2d(2, 2),
        nn.Conv2d(16, 32, kernel_size=3, padding=1),
        nn.ReLU(),
        nn.MaxPool2d(2, 2),
    )
    head: nn.Linear = nn.Linear(32 * 7 * 7, n_classes)
    return SimpleCNN(features=features, head=head)


def count_params(layers: nn.Sequential) -> int:
    return sum((int(p.numel()) for p in layers.parameters() if p.requires_grad))


def initialise_kaiming(seq: nn.Sequential) -> None:
    for m in seq.modules():
        if isinstance(m, nn.Linear) or isinstance(m, nn.Conv2d):
            nn.init.kaiming_normal_(m.weight, nonlinearity="relu")
            if m.bias is not None:
                nn.init.zeros_(m.bias)


def main() -> None:
    mlp: MLP = make_mlp(in_dim=20, hidden=64, out_dim=3)
    initialise_kaiming(mlp.layers)
    print(f"mlp params: {count_params(mlp.layers)}")
    dummy: torch.Tensor = torch.randn(4, 20)
    mlp_out: torch.Tensor = mlp.forward(dummy)
    print(f"mlp output shape: {mlp_out.shape}")
    cnn: SimpleCNN = make_cnn(n_classes=10)
    initialise_kaiming(cnn.features)
    nn.init.kaiming_normal_(cnn.head.weight, nonlinearity="relu")
    nn.init.zeros_(cnn.head.bias)
    img: torch.Tensor = torch.randn(2, 1, 28, 28)
    logits: torch.Tensor = cnn.forward(img)
    print(f"cnn output shape: {logits.shape}")
    print(f"softmax row 0:   {F.softmax(logits, dim=1)[0]}")


if __name__ == "__main__":
    main()
