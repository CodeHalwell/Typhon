# tyc::invalid_config_value

Fires at config-load time when `typhon.toml` declares a value that isn't in
the allowed enumeration for that key. Rejected eagerly so the build never
silently does the wrong thing with a misspelled or unsupported setting.

## Example

```toml
# typhon.toml
[emit]
class-default = "plain"   # error: expected one of `dataclass` | `pydantic`
```

## Why

A typo in a config key would otherwise be ignored, and the build would
continue with the default behaviour — making debugging the "why isn't my
setting taking effect?" question much harder. Failing fast at config-load
time anchors the diagnostic to the offending line.

## Fix

Replace the value with one of the allowed alternatives listed in the error
message:

```toml
[emit]
class-default = "dataclass"
```

See https://typhon.dev/lang/diagnostics/invalid_config_value
