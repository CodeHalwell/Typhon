# tyc::loop_closure_capture

Fires when a closure (lambda or nested `def`) created inside a `for`
loop references the loop variable.

## Example

```ty
def main() -> None:
    mut fns: list[Callable[[], int]] = []
    for i in range(3):
        # warning: every closure shares the single `i` binding
        fns.append(lambda: i)
    print([g() for g in fns])   # [2, 2, 2] — not [0, 1, 2]
```

## Why

Python closures capture *variables*, not values. All three lambdas
share the one `i` binding, and they read it at **call** time — after
the loop has finished, when `i` holds its final value. The classic
silently-wrong-output gotcha.

Immediately-invoked closures (`(lambda: n * n)()`) and closures whose
own parameters shadow the loop name are exempt.

## Fix

Bind the current value per closure with a parameter default (evaluated
at definition time):

```ty
for i in range(3):
    fns.append(lambda i=i: i)
```

Or compute eagerly instead of deferring.
