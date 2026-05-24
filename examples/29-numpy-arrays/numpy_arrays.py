from __future__ import annotations
from typhon_runtime.lazy import lazy_import as __typhon_lazy_import

np = __typhon_lazy_import("numpy")


def demo_creation() -> None:
    zeros = np.zeros((3, 4))
    ones = np.ones(5)
    arange = np.arange(0, 10, 2)
    linspace = np.linspace(0.0, 1.0, 5)
    rand = np.random.default_rng(seed=42).standard_normal((2, 3))
    print(f"zeros shape: {zeros.shape}")
    print(f"arange: {arange}")
    print(f"linspace: {linspace}")
    print(f"rand:\n{rand}")


def demo_vector_ops() -> None:
    a = np.array([1.0, 2.0, 3.0, 4.0])
    b = np.array([10.0, 20.0, 30.0, 40.0])
    print(f"sum:   {a + b}")
    print(f"prod:  {a * b}")
    print(f"dot:   {np.dot(a, b)}")
    print(f"norm:  {np.linalg.norm(a):.3f}")
    print(f"mean:  {a.mean()}, std: {a.std():.3f}")


def demo_broadcasting() -> None:
    m = np.arange(12).reshape(3, 4).astype(np.float64)
    row = np.array([1.0, 2.0, 3.0, 4.0])
    col = np.array([[10.0], [20.0], [30.0]])
    print(f"matrix:\n{m}")
    print(f"+ row:\n{m + row}")
    print(f"* col:\n{m * col}")


def demo_linalg() -> None:
    a = np.array([[3.0, 1.0], [1.0, 2.0]])
    b = np.array([9.0, 8.0])
    x = np.linalg.solve(a, b)
    print(f"Ax = b -> x = {x}")
    eigvals = np.linalg.eigvals(a)
    print(f"eigvals: {eigvals}")


def demo_masking() -> None:
    rng = np.random.default_rng(seed=0)
    data = rng.integers(0, 100, size=20)
    big = data[data > 50]
    print(f"data: {data}")
    print(f"items > 50: {big}")
    print(f"clipped: {np.clip(data, 25, 75)}")


def main() -> None:
    demo_creation()
    demo_vector_ops()
    demo_broadcasting()
    demo_linalg()
    demo_masking()


if __name__ == "__main__":
    main()
