# Third-party argument checking — numpy / pandas / scikit-learn

Verifies that Typhon **fails to compile, naming the missing parameter**, when a
call to a popular third-party library omits a required argument — for
constructors, free functions, **and methods**. This is the
`tyc-venv` introspection layer (`inspect.signature` over the installed
packages) feeding the same `tyc::missing_argument` machinery the project's own
code uses.

## Requirements

The check reads the project's `.venv`, so the libraries must be installed there
(this is also how the `scikit-learn` → `sklearn` dist→import name resolution
works — via `.dist-info` metadata):

```bash
python3.13 -m venv .venv
.venv/bin/pip install numpy pandas scikit-learn
```

`harness.sh` creates `proj/.venv` and installs the three libraries on first run
if it isn't already present. **`proj/` (and its `.venv`) is generated and must
not be committed.**

```bash
bash harness.sh        # TYC=/path/to/tyc overridable
```

## What it checks

`must_fail/*.ty` — each omits a required argument and **must** fail
`tyc check` with `tyc::missing_argument`:

| File | Call | Missing param |
|---|---|---|
| `01-pca-fit-missing-X` | `PCA(n_components=2).fit()` | `X` (**method**) |
| `02-scaler-transform-missing-X` | `StandardScaler().transform()` | `X` (**method**) |
| `03-pipeline-missing-steps` | `Pipeline()` | `steps` (constructor) |
| `04-merge-missing-right` | `df.merge()` | `right` (**method**) |
| `05-accuracy-missing-ypred` | `accuracy_score([...])` | `y_pred` (function) |
| `06-np-zeros-missing-shape` | `np.zeros()` | `shape` (function) |

`must_pass/*.ty` — idiomatic, correct usage of a realistic numpy + pandas +
scikit-learn pipeline that **must** type-check clean (guards against false
positives).

## Result (v0.15.6 + Unreleased method-introspection)

```
must_fail: caught=6 missed=0 | must_pass: clean=2 false_positive=0
```

Before the Unreleased change, the three **method** cases (`fit`, `transform`,
`merge`) compiled silently — only constructors and free functions were
introspected. Now method calls are arity-checked too. The constructor/function
cases (`Pipeline`, `accuracy_score`, `np.zeros`) were already caught.

### Design notes (why this stays false-positive-free)

- Only methods whose signature `inspect.signature` can recover are captured;
  C-extension slots (most `numpy.ndarray` methods) are skipped and stay
  lenient (the class shape remains `partial`).
- The implicit `self` / `cls` is stripped only when it's a genuine leading
  positional — a decorator-wrapped `(*args, **kwargs)` method (common in
  scikit-learn) is left fully permissive instead of having its `*args`
  mis-stripped into a bogus positional cap.
- `**kwargs` / `*args` methods keep an unbounded maximum, so only the genuine
  *minimum* required positionals are enforced.
