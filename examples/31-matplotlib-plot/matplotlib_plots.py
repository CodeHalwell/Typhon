from __future__ import annotations
from typhon_runtime.lazy import lazy_import as __typhon_lazy_import
from pathlib import Path

np = __typhon_lazy_import("numpy")
plt = __typhon_lazy_import("matplotlib.pyplot")


def line_plot(out: Path) -> None:
    x = np.linspace(0.0, 4.0 * np.pi, 200)
    y1 = np.sin(x)
    y2 = np.cos(x)
    (fig, ax) = plt.subplots(figsize=(8.0, 4.0))
    ax.plot(x, y1, label="sin", linewidth=2.0)
    ax.plot(x, y2, label="cos", linewidth=2.0, linestyle="--")
    ax.set_title("trig functions")
    ax.set_xlabel("radians")
    ax.set_ylabel("amplitude")
    ax.legend(loc="upper right")
    ax.grid(alpha=0.3)
    fig.tight_layout()
    fig.savefig(str(out), dpi=120)
    plt.close(fig)


def bar_plot(out: Path, categories: list[str], values: list[float]) -> None:
    (fig, ax) = plt.subplots(figsize=(7.0, 4.0))
    ax.bar(categories, values, color="#3F88C5")
    ax.set_title("revenue by product")
    ax.set_ylabel("USD")
    for i, v in enumerate(values):
        ax.text(i, v + 0.5, f"${v:.0f}", ha="center", fontsize=9)
    fig.tight_layout()
    fig.savefig(str(out), dpi=120)
    plt.close(fig)


def scatter_with_fit(out: Path) -> None:
    rng = np.random.default_rng(seed=42)
    x = rng.uniform(0.0, 10.0, 80)
    noise = rng.normal(0.0, 1.5, 80)
    y = 2.0 * x + 1.0 + noise
    coeffs = np.polyfit(x, y, 1)
    x_line = np.linspace(0.0, 10.0, 50)
    y_line = np.polyval(coeffs, x_line)
    (fig, ax) = plt.subplots(figsize=(7.0, 5.0))
    ax.scatter(x, y, alpha=0.6, s=30, label="data")
    ax.plot(
        x_line, y_line, color="red", label=f"fit y={coeffs[0]:.2f}x+{coeffs[1]:.2f}"
    )
    ax.set_title("scatter with linear fit")
    ax.legend()
    fig.tight_layout()
    fig.savefig(str(out), dpi=120)
    plt.close(fig)


def main() -> None:
    out_dir: Path = Path("/tmp/typhon-plots")
    out_dir.mkdir(parents=True, exist_ok=True)
    line_plot(out_dir / "trig.png")
    bar_plot(
        out_dir / "revenue.png",
        ["widget", "gadget", "thingy", "gizmo"],
        [29.97, 199.96, 72.5, 14.99],
    )
    scatter_with_fit(out_dir / "scatter.png")
    print(f"plots written to {out_dir}/")


if __name__ == "__main__":
    main()
