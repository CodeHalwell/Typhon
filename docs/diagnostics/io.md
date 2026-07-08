# tyc::io

Fires when the compiler can't read a source file from disk — the path
doesn't exist, isn't readable, or the OS returned some other I/O error.

## Example

```text
tyc check missing.ty
# error: could not read file 'missing.ty': No such file or directory
```

## Why

The compiler needs to load every source file referenced by a build before
parsing or checking can begin. A missing or unreadable file is a hard stop:
without the bytes there's nothing to compile.

## Fix

Verify the path exists, check filesystem permissions, and re-run the
command. If the file was deleted or moved, update the import (or the
project layout) to match.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/io.md
