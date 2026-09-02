//! Foreign-function fallback boundary.
//!
//! v1: a stub. Anything that needs CPython embedding (numpy, requests,
//! pydantic, etc.) currently returns a clear `ImportError` pointing the user
//! at `tyc run --compile`. A future `vm-pyo3` feature will replace these
//! stubs with PyO3-backed shims.
//!
//! The text/binary file objects that used to live here are now the `io`
//! shim (`shims/io.py`) over the `_fs_*` natives in `builtins`.
