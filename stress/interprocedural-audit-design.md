# Inter-Procedural Field-Init Audit Design

**Status:** Design proposal for Epic sub-item 3

## Current State (PR #105)

The existing audit tracks a **narrow pattern**:
```python
def make_partial() -> ApiClient:
    return ApiClient.__new__(ApiClient)

let c = make_partial()  # Tracked: c has missing fields
use(c)                   # Error: tyc::missing_field_init
```

**Limitations:**
1. Only recognizes `return X.__new__(X)` (literal pattern)
2. Helpers that do partial initialization aren't tracked
3. No composition support for multi-step factories
4. Parameter-based partial instances aren't tracked

## Proposed Summary IR

### Per-Function Summary

```rust
struct FunctionSummary {
    /// Function name for debugging
    name: String,

    /// Parameters that might be partial instances
    /// Maps param_index → (class_name, fields_required_after_call)
    partial_params: HashMap<usize, PartialParamInfo>,

    /// Return paths and their partial-instance status
    /// Maps return_path_id → UninitInstance (class + missing fields)
    partial_returns: HashMap<usize, UninitInstance>,

    /// Fields assigned by this function on each parameter
    /// Maps param_index → Set<field_name>
    param_field_assigns: HashMap<usize, HashSet<String>>,
}

struct PartialParamInfo {
    class: String,
    /// Fields that MUST be assigned after calling this function
    /// (fields not assigned by the function body)
    missing_after: HashSet<String>,
}
```

### Example 1: Helper that finishes initialization

```python
class Config:
    host: str
    port: int
    timeout: int = 30  # has default

def init_network(cfg: Config) -> None:
    cfg.host = "localhost"
    cfg.port = 8080
    # timeout has default, not required

# Usage:
let c = Config.__new__(Config)  # Missing: {host, port}
init_network(c)                  # After: {} (fully initialized)
use(c)                           # OK
```

**Summary for `init_network`:**
```rust
FunctionSummary {
    name: "init_network",
    partial_params: {
        0: PartialParamInfo {
            class: "Config",
            missing_after: {},  // Assigns host, port (all required fields)
        }
    },
    partial_returns: {},
    param_field_assigns: {
        0: {"host", "port"}
    },
}
```

### Example 2: Multi-step factory chain

```python
def make_partial() -> Config:
    return Config.__new__(Config)

def init_basic(cfg: Config) -> None:
    cfg.host = "localhost"

def finish_config(cfg: Config) -> None:
    cfg.port = 8080

# Usage:
let c = make_partial()  # Missing: {host, port}
init_basic(c)            # After: {port}
finish_config(c)         # After: {}
use(c)                   # OK
```

**Summary for `init_basic`:**
```rust
FunctionSummary {
    name: "init_basic",
    partial_params: {
        0: PartialParamInfo {
            class: "Config",
            missing_after: {"port"},  // Only assigns host
        }
    },
    // ...
}
```

**Summary for `finish_config`:**
```rust
FunctionSummary {
    name: "finish_config",
    partial_params: {
        0: PartialParamInfo {
            class: "Config",
            missing_after: {},  // Assigns port (last required field)
        }
    },
    // ...
}
```

### Example 3: Helper returns partial

```python
def make_with_host() -> Config:
    let c = Config.__new__(Config)
    c.host = "localhost"
    return c

# Usage:
let c = make_with_host()  # Missing: {port}
c.port = 8080              # After: {}
use(c)                     # OK
```

**Summary for `make_with_host`:**
```rust
FunctionSummary {
    name: "make_with_host",
    partial_params: {},
    partial_returns: {
        0: UninitInstance {
            class: "Config",
            missing: {"port"},  // Assigns host but not port
        }
    },
    // ...
}
```

## Implementation Plan

### Phase 1: Compute Summaries

**New analysis pass** in `tyc-types/src/lib.rs`:

```rust
fn compute_function_summary(c: &Checker, func: &FunctionDef) -> FunctionSummary {
    // 1. Find all `X.__new__(X)` constructions
    // 2. Track field assignments (c.field = ...)
    // 3. Track return statements with partial instances
    // 4. Compute missing sets for each return path
    // 5. For each param of class type, track which fields are assigned

    FunctionSummary { /* ... */ }
}
```

Add to `Checker`:
```rust
/// Per-function summaries, keyed by function name
function_summaries: HashMap<String, FunctionSummary>,
```

Populate during the initial prescan (after `prescan_partial_returning_fns`):
```rust
fn prescan_function_summaries(c: &mut Checker, body: &[Stmt]) {
    for stmt in body {
        let Stmt::FunctionDef(f) = stmt else { continue };
        let summary = compute_function_summary(c, f);
        c.function_summaries.insert(f.name.to_string(), summary);
    }
}
```

### Phase 2: Consume Summaries at Call Sites

**Update `audit_register_bypass`** to check summaries:

```rust
// When assigning result of a call:
let c = some_function(...)

// Look up summary for `some_function`
if let Some(summary) = c.function_summaries.get("some_function") {
    // Check partial_returns to see if result is partial
    for (_, uninit) in &summary.partial_returns {
        c.uninit_instances.insert(lhs_name, uninit.clone());
    }
}
```

**Update call-site argument tracking:**

```rust
// When calling a function with a tracked partial instance:
some_function(c)

// Look up summary for `some_function`
if let Some(summary) = c.function_summaries.get("some_function") {
    if let Some(param_info) = summary.partial_params.get(&0) {  // First param
        // Update tracked instance's missing set
        if let Some(uninit) = c.uninit_instances.get_mut("c") {
            uninit.missing = param_info.missing_after.clone();
            // If now empty, remove from tracking
            if uninit.missing.is_empty() {
                c.uninit_instances.remove("c");
            }
        }
    }
}
```

### Phase 3: Tests

Add to `tyc-types` test suite:

```rust
#[test]
fn interprocedural_helper_finishes_init() {
    // Test helper that assigns all remaining fields
}

#[test]
fn interprocedural_multi_step_chain() {
    // Test make_partial() → init_basic() → finish_config()
}

#[test]
fn interprocedural_helper_returns_partial() {
    // Test helper that returns partially-initialized instance
}

#[test]
fn interprocedural_param_passthrough() {
    // Test function that receives partial, doesn't assign, passes to another
}
```

## Limitations (Carry-Forward)

1. **Intra-module only**: Summaries aren't serialized, so cross-module helpers aren't tracked
2. **Simple data flow**: No SSA or aliasing analysis; assumes straight-line field assignments
3. **No control flow merging**: If/else branches with different field assignments aren't unified
4. **Conservative on complexity**: Any dynamic assignment (`setattr`, loops assigning fields) drops tracking

## Integration Points

- **Prescan phase**: After class shapes, before main check
- **Call sites**: `check_expr` when encountering `Expr::Call`
- **Return sites**: `check_stmt` for `Stmt::Return`
- **Assignment sites**: `check_stmt` for `Stmt::Assign` / `Stmt::AnnAssign`

## Success Criteria

1. Helper that finishes initialization doesn't trigger `tyc::missing_field_init`
2. Multi-step factory chain correctly tracks missing fields at each step
3. Helper returning partial instance triggers error at escape point
4. Regression: Existing trivial-factory pattern still works
