from __future__ import annotations
from typhon_runtime.lazy import lazy_import as __typhon_lazy_import

torch = __typhon_lazy_import("torch")


def pick_device() -> torch.device:
    if torch.cuda.is_available():
        return torch.device("cuda")
    if torch.backends.mps.is_available():
        return torch.device("mps")
    return torch.device("cpu")


def demo_creation(device: torch.device) -> None:
    zeros: torch.Tensor = torch.zeros(2, 3, device=device)
    ones: torch.Tensor = torch.ones(3, device=device)
    arange: torch.Tensor = torch.arange(0, 10, 2, device=device)
    randn: torch.Tensor = torch.randn(2, 3, device=device)
    print(f"zeros: {zeros.shape}")
    print(f"ones: {ones}")
    print(f"arange: {arange}")
    print(f"randn:\n{randn}")


def demo_ops(device: torch.device) -> None:
    a: torch.Tensor = torch.tensor([[1.0, 2.0], [3.0, 4.0]], device=device)
    b: torch.Tensor = torch.tensor([[5.0, 6.0], [7.0, 8.0]], device=device)
    print(f"a + b:\n{a + b}")
    print(f"a @ b:\n{a @ b}")
    print(f"a.sum(dim=0): {a.sum(dim=0)}")
    print(f"a.mean(): {a.mean().item():.3f}")


def demo_autograd() -> None:
    x: torch.Tensor = torch.tensor([2.0, 3.0], requires_grad=True)
    y: torch.Tensor = x.pow(2).sum() + 4.0 * x.sum()
    y.backward()
    print(f"y = {y.item()}")
    print(f"dy/dx = {x.grad}")


def demo_reshape(device: torch.device) -> None:
    t: torch.Tensor = torch.arange(24, device=device).reshape(2, 3, 4)
    print(f"t.shape: {t.shape}")
    print(f"permute(0,2,1).shape: {t.permute(0, 2, 1).shape}")
    print(f"flatten: {t.flatten().shape}")
    print(f"squeeze: {t.unsqueeze(0).squeeze().shape}")


def main() -> None:
    device: torch.device = pick_device()
    print(f"using {device}")
    demo_creation(device)
    demo_ops(device)
    demo_autograd()
    demo_reshape(device)


if __name__ == "__main__":
    main()
