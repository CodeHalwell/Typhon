# Installing Typhon

This guide covers installing the `tyc` compiler binary on **macOS,
Linux, and Windows**. Pre-built binaries ship on every GitHub Release
since v0.3.0; the current release is
[v0.8.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.8.0).
If your platform isn't listed (FreeBSD, Linux MUSL, Windows ARM64),
build from source — see the
[Quick start](../README.md#quick-start) section of the README.

## Supported platforms

| OS      | Architecture | Target triple                  | Archive |
|---------|--------------|--------------------------------|---------|
| macOS   | Apple Silicon (`arm64`) | `aarch64-apple-darwin`         | `.tar.gz` |
| macOS   | Intel (`x86_64`)        | `x86_64-apple-darwin`          | `.tar.gz` |
| Linux   | `x86_64`                | `x86_64-unknown-linux-gnu`     | `.tar.gz` |
| Linux   | `aarch64`               | `aarch64-unknown-linux-gnu`    | `.tar.gz` |
| Windows | `x86_64`                | `x86_64-pc-windows-msvc`       | `.zip`    |

Linux artifacts are built on Ubuntu 22.04 against glibc 2.35; they run
on any reasonably modern glibc-based distro (Ubuntu 22.04+, Debian 12+,
Fedora 36+, RHEL 9+, Arch). For MUSL-based distros (Alpine), either run
under `gcompat` or build from source.

## Prerequisites

- **CPython 3.13 or newer** at runtime. **This is a hard requirement** —
  Typhon rejects any `[python] target` below 3.13 at config-load time
  with `unsupported [python] target`. Valid values are `"3.13"`,
  `"3.13t"` (free-threaded), `"3.14"`, `"3.14t"`. The bundled `init`
  scaffold defaults to `"3.13"`. The emitted code is plain CPython —
  there is no Typhon runtime to install.
- macOS / Linux: `curl`, `tar`, and `shasum` or `sha256sum`. All ship
  with the platform by default; on minimal Linux containers you may
  need to install `tar` and `coreutils`.
- Windows: PowerShell 5.1+ (built into Windows 10/11). `iwr` / `iex`
  are aliases for `Invoke-WebRequest` / `Invoke-Expression`.

> The `tyc` binary itself doesn't need Python installed to compile your
> code. You only need Python to *run* the emitted `.py` output.

## One-line install

### macOS / Linux

```bash
curl -sSL https://raw.githubusercontent.com/codehalwell/typhon/main/install.sh | sh
```

The installer:

1. Detects your OS + CPU architecture and chooses the matching tarball
   (`aarch64-apple-darwin`, `x86_64-apple-darwin`,
   `x86_64-unknown-linux-gnu`, or `aarch64-unknown-linux-gnu`).
2. Resolves the latest GitHub Release tag via the GitHub API.
3. Downloads `tyc-<version>-<target>.tar.gz` and the matching
   `SHA256SUMS` file from the release assets.
4. Verifies the SHA-256 checksum with `shasum -a 256 -c` (macOS) or
   `sha256sum -c` (Linux).
5. Extracts the tarball and installs `tyc` to `$HOME/.local/bin` by
   default (no `sudo` needed).
6. On macOS only: clears the `com.apple.quarantine` extended attribute
   so Gatekeeper doesn't prompt you on first run.

Re-running the script upgrades to the latest release in place.

### Windows (PowerShell)

```powershell
iwr -useb https://raw.githubusercontent.com/codehalwell/typhon/main/install.ps1 | iex
```

The installer:

1. Detects your CPU architecture and chooses the matching zip
   (`x86_64-pc-windows-msvc` today; ARM64 is not yet a pre-built
   target — build from source).
2. Resolves the latest GitHub Release tag via the GitHub API.
3. Downloads `tyc-<version>-<target>.zip` and the matching
   `SHA256SUMS` file from the release assets.
4. Verifies the SHA-256 checksum using `Get-FileHash`.
5. Extracts the zip and installs `tyc.exe` to
   `%LOCALAPPDATA%\Programs\Typhon\` by default (no admin rights
   needed).
6. Adds the install directory to your **user-level** `PATH` via
   `[Environment]::SetEnvironmentVariable`. Open a new terminal for
   the PATH change to take effect.

Re-running the script upgrades to the latest release in place.

### Pinning a version

macOS / Linux:

```bash
TYPHON_VERSION=v0.8.0 \
  curl -sSL https://raw.githubusercontent.com/codehalwell/typhon/main/install.sh | sh
```

Or with the script saved locally:

```bash
sh install.sh --version=v0.8.0
```

Windows (PowerShell):

```powershell
$env:TYPHON_VERSION = 'v0.8.0'
iwr -useb https://raw.githubusercontent.com/codehalwell/typhon/main/install.ps1 | iex
```

Or with the script saved locally:

```powershell
.\install.ps1 -Version v0.8.0
```

### Custom install directory

macOS / Linux:

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

Windows (PowerShell):

```powershell
.\install.ps1 -InstallDir C:\Tools\Typhon
```

Or:

```powershell
$env:TYPHON_INSTALL_DIR = 'C:\Tools\Typhon'
iwr -useb https://raw.githubusercontent.com/codehalwell/typhon/main/install.ps1 | iex
```

Use `-NoPath` to skip the user-PATH update if you'd rather manage that
yourself.

## Manual download

If you'd rather not pipe a script into `sh` or `iex`, grab the archive
directly from the
[releases page](https://github.com/codehalwell/typhon/releases/latest).

1. Pick the asset matching your platform:
   - Apple Silicon: `tyc-<version>-aarch64-apple-darwin.tar.gz`
   - Intel macOS:   `tyc-<version>-x86_64-apple-darwin.tar.gz`
   - Linux x86_64:  `tyc-<version>-x86_64-unknown-linux-gnu.tar.gz`
   - Linux aarch64: `tyc-<version>-aarch64-unknown-linux-gnu.tar.gz`
   - Windows:       `tyc-<version>-x86_64-pc-windows-msvc.zip`
2. Download the archive and the `SHA256SUMS` file.
3. Verify the checksum:

   macOS / Linux:

   ```bash
   # macOS: shasum -a 256 -c SHA256SUMS --ignore-missing
   sha256sum -c SHA256SUMS --ignore-missing
   ```

   Windows:

   ```powershell
   $expected = (Get-Content SHA256SUMS | Where-Object { $_ -match 'tyc-.*\.zip$' }) -split '\s+' | Select-Object -First 1
   $actual   = (Get-FileHash tyc-*-x86_64-pc-windows-msvc.zip -Algorithm SHA256).Hash.ToLower()
   if ($expected.ToLower() -eq $actual) { 'OK' } else { 'MISMATCH' }
   ```

4. Extract and move the binary onto your `PATH`:

   macOS / Linux:

   ```bash
   tar -xzf tyc-<version>-<target>.tar.gz
   mkdir -p "$HOME/.local/bin"
   mv tyc-<version>-<target>/tyc "$HOME/.local/bin/tyc"
   chmod +x "$HOME/.local/bin/tyc"
   ```

   Windows (PowerShell):

   ```powershell
   Expand-Archive tyc-<version>-x86_64-pc-windows-msvc.zip -DestinationPath .
   $install = "$env:LOCALAPPDATA\Programs\Typhon"
   New-Item -ItemType Directory -Path $install -Force | Out-Null
   Copy-Item tyc-*-x86_64-pc-windows-msvc\tyc.exe $install
   ```

5. **macOS only — clear the Gatekeeper quarantine attribute.** macOS
   adds this to anything downloaded by a browser; the install script
   handles this automatically but a manual download does not:

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

### macOS / Linux

```bash
# bash — append to ~/.bashrc or ~/.bash_profile
export PATH="$HOME/.local/bin:$PATH"

# zsh — append to ~/.zshrc
export PATH="$HOME/.local/bin:$PATH"

# fish — append to ~/.config/fish/config.fish
fish_add_path "$HOME/.local/bin"
```

Restart your shell or `source` the rc file, then check:

```bash
which tyc
tyc --version
```

### Windows

The PowerShell installer adds the install directory to your **user**
PATH automatically. To do it manually:

```powershell
$dir = "$env:LOCALAPPDATA\Programs\Typhon"
$current = [Environment]::GetEnvironmentVariable('PATH', 'User')
[Environment]::SetEnvironmentVariable('PATH', "$current;$dir", 'User')
```

Open a new terminal for the change to take effect, then check:

```powershell
where.exe tyc
tyc --version
```

## Uninstalling

### macOS / Linux

```bash
rm "$HOME/.local/bin/tyc"
```

(Or remove from whichever `--dir` you installed into.) No other files
are written outside the install directory.

### Windows

```powershell
Remove-Item "$env:LOCALAPPDATA\Programs\Typhon\tyc.exe"
# Optional: remove the now-empty directory and the user-PATH entry.
$dir = "$env:LOCALAPPDATA\Programs\Typhon"
if (Test-Path $dir) { Remove-Item $dir -Recurse }
$userPath = [Environment]::GetEnvironmentVariable('PATH', 'User')
$cleaned = ($userPath -split ';') | Where-Object { $_ -and ($_.TrimEnd('\') -ine $dir.TrimEnd('\')) }
[Environment]::SetEnvironmentVariable('PATH', ($cleaned -join ';'), 'User')
```

## Code signing

### macOS

Typhon does not yet have a paid Apple Developer certificate, so macOS
releases are **ad-hoc signed** (`codesign --sign -`). Ad-hoc signatures
avoid the silent `killed: 9` failure mode that unsigned binaries hit on
Apple Silicon, but they do not establish trust with Gatekeeper.

In practice:

- If you install via the script, the script clears the quarantine xattr
  and Gatekeeper stays quiet.
- If you download manually, macOS will block the first run until you
  either clear the quarantine xattr (see above) or right-click → Open
  in Finder once.

### Windows

`tyc.exe` is **not yet Authenticode-signed**. SmartScreen may show a
"Windows protected your PC" warning on first run after a manual
download — click **More info** → **Run anyway**. The PowerShell
installer downloads via `Invoke-WebRequest`, which doesn't set the
Mark-of-the-Web on the extracted exe, so the warning typically doesn't
appear when you install through the script.

### Linux

Linux binaries are unsigned. Verify the SHA-256 (the installer does this
automatically) and you're good.

## Troubleshooting

### `tyc: command not found` after install

The install directory isn't on your `PATH`. See [PATH setup](#path-setup)
above. The script always prints the directory it installed to as its
last "Installed …" line. On Windows, open a new terminal — the PATH
change does not propagate to existing sessions.

### `"tyc" cannot be opened because the developer cannot be verified`

(macOS only.) This is the Gatekeeper prompt. Clear the quarantine
attribute and retry:

```bash
xattr -d com.apple.quarantine "$HOME/.local/bin/tyc"
```

### `killed: 9` on first run

(macOS only.) This indicates the binary isn't signed (or the signature
is corrupt). Release builds are ad-hoc signed in CI; if you see this on
a release artifact, please open an issue with the tag and your
`uname -m` output.

### `bad ELF interpreter` / `/lib64/ld-linux-x86-64.so.2: not found`

(Linux only.) The release tarball is a dynamically-linked glibc binary.
This error means you're on a MUSL distro (Alpine) or an extremely old
glibc. Workarounds:

- Alpine: install the `gcompat` package (`apk add gcompat`).
- Old glibc: build from source against your toolchain.

### Wrong architecture

If `tyc` exits immediately with `Bad CPU type in executable` (macOS),
`Exec format error` (Linux), or `not a valid Win32 application`
(Windows), you've installed the binary for the other architecture.

```bash
# macOS / Linux
uname -m
# arm64 / aarch64  → install the aarch64-* tarball
# x86_64 / amd64   → install the x86_64-* tarball
```

```powershell
# Windows
$env:PROCESSOR_ARCHITECTURE
# AMD64 → install the x86_64-pc-windows-msvc zip
```

The installers do this detection automatically; manual downloads must
be matched by hand.

### Behind a corporate proxy / firewall

The POSIX installer uses `curl`; set `HTTPS_PROXY` / `HTTP_PROXY` in
the environment and curl picks them up automatically.

The PowerShell installer uses `Invoke-WebRequest`; it honours the
`HTTPS_PROXY` env var when present, or set the proxy explicitly:

```powershell
[Net.WebRequest]::DefaultWebProxy = New-Object Net.WebProxy('http://proxy.example.com:8080', $true)
```

### Checksum mismatch

If the install script aborts on the SHA-256 step, the download was
corrupted or tampered with. Re-run the installer; if it persists,
download the archive manually from the releases page and re-verify
against the published `SHA256SUMS`.

## Related

- [README — Install](../README.md#install)
- [`docs/cli.md`](cli.md) — full `tyc` subcommand reference
- [`docs/configuration.md`](configuration.md) — `typhon.toml` reference
