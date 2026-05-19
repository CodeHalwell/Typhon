# Installing Typhon

This guide covers installing the `tyc` compiler binary on macOS. For other
platforms, build from source (see the [Quick start](../README.md#quick-start)
section of the README).

## Prerequisites

- **macOS** on Apple Silicon (`arm64`) or Intel (`x86_64`).
- **CPython 3.13 or newer** at runtime. Typhon emits Python that targets
  3.13+ by default (configurable via `[python] target` in `typhon.toml`).
  The default in the bundled `init` scaffold is `target = "3.13"`. The
  emitted code is plain CPython — there is no Typhon runtime to install.
- `curl`, `tar`, and `shasum`. All ship with macOS by default.

> The `tyc` binary itself doesn't need Python installed to compile your
> code. You only need Python to *run* the emitted `.py` output.

## One-line install (recommended)

```bash
curl -sSL https://raw.githubusercontent.com/codehalwell/typhon/main/install.sh | sh
```

The installer:

1. Detects your CPU architecture (`arm64` → `aarch64-apple-darwin`,
   `x86_64` → `x86_64-apple-darwin`).
2. Resolves the latest GitHub Release tag via the GitHub API.
3. Downloads `tyc-<version>-<target>.tar.gz` and the matching
   `SHA256SUMS` file from the release assets.
4. Verifies the SHA-256 checksum with `shasum -a 256 -c`.
5. Extracts the tarball and installs `tyc` to `$HOME/.local/bin` by
   default (no `sudo` needed).
6. Clears the `com.apple.quarantine` extended attribute so Gatekeeper
   doesn't prompt you on first run.

Re-running the script upgrades to the latest release in place.

### Pinning a version

```bash
TYPHON_VERSION=v0.1.0 \
  curl -sSL https://raw.githubusercontent.com/codehalwell/typhon/main/install.sh | sh
```

Or, with the script saved locally:

```bash
sh install.sh --version=v0.1.0
```

### Custom install directory

```bash
TYPHON_INSTALL_DIR=/opt/typhon/bin \
  curl -sSL https://raw.githubusercontent.com/codehalwell/typhon/main/install.sh | sh
```

Or:

```bash
sh install.sh --dir=/opt/typhon/bin
```

Both forms require write access to the target directory. The default
(`$HOME/.local/bin`) is per-user and never needs `sudo`.

## Manual download

If you'd rather not pipe a script into `sh`, grab the tarball directly
from the [releases page](https://github.com/codehalwell/typhon/releases/latest).

1. Pick the asset matching your CPU:
   - Apple Silicon: `tyc-<version>-aarch64-apple-darwin.tar.gz`
   - Intel: `tyc-<version>-x86_64-apple-darwin.tar.gz`
2. Download the tarball and the `SHA256SUMS` file.
3. Verify the checksum:

   ```bash
   shasum -a 256 -c SHA256SUMS --ignore-missing
   ```

4. Extract and move the binary onto your `PATH`:

   ```bash
   tar -xzf tyc-<version>-<target>.tar.gz
   mkdir -p "$HOME/.local/bin"
   mv tyc-<version>-<target>/tyc "$HOME/.local/bin/tyc"
   chmod +x "$HOME/.local/bin/tyc"
   ```

5. **Clear the Gatekeeper quarantine attribute.** macOS adds this to
   anything downloaded by a browser; the install script handles this
   automatically but a manual download does not:

   ```bash
   xattr -d com.apple.quarantine "$HOME/.local/bin/tyc"
   ```

6. Confirm it works:

   ```bash
   tyc --version
   ```

## PATH setup

The installer prints a hint if the install directory isn't on your
`PATH`. To add it manually:

```bash
# bash — append to ~/.bashrc or ~/.bash_profile
export PATH="$HOME/.local/bin:$PATH"

# zsh — append to ~/.zshrc
export PATH="$HOME/.local/bin:$PATH"
```

Restart your shell or `source` the rc file, then check:

```bash
which tyc
tyc --version
```

## Uninstalling

```bash
rm "$HOME/.local/bin/tyc"
```

(Or remove from whichever `--dir` you installed into.) No other files
are written outside the install directory.

## Code signing

Typhon does not yet have a paid Apple Developer certificate, so releases
are **ad-hoc signed** (`codesign --sign -`). Ad-hoc signatures avoid the
silent `killed: 9` failure mode that unsigned binaries hit on Apple
Silicon, but they do not establish trust with Gatekeeper.

In practice:

- If you install via the script, the script clears the quarantine xattr
  and Gatekeeper stays quiet.
- If you download manually, macOS will block the first run until you
  either clear the quarantine xattr (see above) or right-click → Open
  in Finder once.

## Troubleshooting

### `tyc: command not found` after install

The install directory isn't on your `PATH`. See [PATH setup](#path-setup)
above. The script always prints the directory it installed to as its
last "Installed …" line.

### `"tyc" cannot be opened because the developer cannot be verified`

This is the Gatekeeper prompt. Clear the quarantine attribute and retry:

```bash
xattr -d com.apple.quarantine "$HOME/.local/bin/tyc"
```

### `killed: 9` on first run

This indicates the binary isn't signed (or the signature is corrupt).
Release builds are ad-hoc signed in CI; if you see this on a release
artifact, please open an issue with the tag and your `uname -m` output.

### Wrong architecture

If `tyc` exits immediately with `Bad CPU type in executable`, you've
installed the binary for the other architecture. Confirm `uname -m`:

```bash
uname -m
# arm64  → install the aarch64-apple-darwin tarball
# x86_64 → install the x86_64-apple-darwin tarball
```

The script does this detection automatically; manual downloads must be
matched by hand.

### Behind a corporate proxy / firewall

The install script uses `curl` for all network access. Set
`HTTPS_PROXY` / `HTTP_PROXY` in the environment and it will be picked
up automatically.

### Checksum mismatch

If the script aborts on the `shasum -a 256 -c` step, the download was
corrupted or tampered with. Re-run the installer; if it persists,
download the tarball manually from the releases page and re-verify.

## Related

- [README — Install on macOS](../README.md#install-on-macos)
- [`docs/cli.md`](cli.md) — full `tyc` subcommand reference
- [`docs/configuration.md`](configuration.md) — `typhon.toml` reference
