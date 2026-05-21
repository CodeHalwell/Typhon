#!/bin/sh
# install.sh — macOS / Linux installer for the Typhon compiler (`tyc`).
#
# Usage:
#   curl -sSL https://raw.githubusercontent.com/codehalwell/typhon/main/install.sh | sh
#
# Flags:
#   --help                 Show this help
#   --version=v0.x.y       Install a specific release tag (default: latest)
#   --dir=/some/path       Install directory (default: $HOME/.local/bin)
#
# Env vars (equivalent to flags):
#   TYPHON_VERSION=v0.x.y
#   TYPHON_INSTALL_DIR=/some/path
#
# This script is POSIX-sh and verbose; it prints what it's about to do
# before each step.

set -eu

REPO="codehalwell/typhon"
DEFAULT_DIR="$HOME/.local/bin"

# ---------------------------------------------------------------------------
# Defaults / arg parsing
# ---------------------------------------------------------------------------

version="${TYPHON_VERSION:-}"
install_dir="${TYPHON_INSTALL_DIR:-$DEFAULT_DIR}"

print_help() {
    cat <<'EOF'
install.sh — install the Typhon compiler (`tyc`) on macOS or Linux.

Usage:
  curl -sSL https://raw.githubusercontent.com/codehalwell/typhon/main/install.sh | sh
  ./install.sh [--version=vX.Y.Z] [--dir=/path]

Supported platforms:
  - macOS:  arm64 (Apple Silicon) and x86_64 (Intel)
  - Linux:  x86_64 and aarch64, glibc-based distros (Ubuntu, Debian,
            Fedora, RHEL, Arch, Alpine via gcompat, etc.)

For Windows, use install.ps1 from PowerShell — see docs/install.md.

Options:
  --help               Show this help and exit
  --version=vX.Y.Z     Install a specific release tag (default: latest)
  --dir=/path          Install directory (default: $HOME/.local/bin)

Environment variables:
  TYPHON_VERSION       Same as --version
  TYPHON_INSTALL_DIR   Same as --dir

Examples:
  # Install latest release
  curl -sSL https://raw.githubusercontent.com/codehalwell/typhon/main/install.sh | sh

  # Install a specific version into a custom directory
  TYPHON_VERSION=v0.3.0 TYPHON_INSTALL_DIR=/opt/typhon/bin sh install.sh
EOF
}

for arg in "$@"; do
    case "$arg" in
        --help|-h)
            print_help
            exit 0
            ;;
        --version=*)
            version="${arg#--version=}"
            ;;
        --dir=*)
            install_dir="${arg#--dir=}"
            ;;
        *)
            printf 'install.sh: unknown argument: %s\n' "$arg" >&2
            printf 'Run with --help for usage.\n' >&2
            exit 2
            ;;
    esac
done

say() {
    printf '==> %s\n' "$*"
}

warn() {
    printf 'warning: %s\n' "$*" >&2
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        die "required command not found: $1"
    fi
}

# ---------------------------------------------------------------------------
# Platform detection
# ---------------------------------------------------------------------------

say "Detecting platform"

os="$(uname -s)"
raw_arch="$(uname -m)"

case "$os" in
    Darwin)
        case "$raw_arch" in
            arm64|aarch64)
                arch="aarch64"
                ;;
            x86_64|amd64)
                arch="x86_64"
                ;;
            *)
                die "unsupported macOS architecture: $raw_arch (expected arm64 or x86_64)"
                ;;
        esac
        target="${arch}-apple-darwin"
        is_linux=0
        ;;
    Linux)
        case "$raw_arch" in
            aarch64|arm64)
                arch="aarch64"
                ;;
            x86_64|amd64)
                arch="x86_64"
                ;;
            *)
                die "unsupported Linux architecture: $raw_arch (expected x86_64 or aarch64)"
                ;;
        esac
        target="${arch}-unknown-linux-gnu"
        is_linux=1
        ;;
    MINGW*|MSYS*|CYGWIN*)
        die "this is a POSIX installer; on Windows, use install.ps1 from PowerShell.
See https://github.com/$REPO/blob/main/docs/install.md for instructions."
        ;;
    *)
        die "unsupported OS: $os (supported: Darwin, Linux).
For other platforms, build from source: https://github.com/$REPO"
        ;;
esac

say "Platform: $os / $raw_arch -> target triple $target"

# ---------------------------------------------------------------------------
# Tool checks
# ---------------------------------------------------------------------------

need_cmd curl
need_cmd tar
need_cmd mktemp
need_cmd uname

# `shasum` ships on macOS by default. On Linux it's typically not
# installed and the equivalent is `sha256sum` (coreutils). Pick whichever
# is present; both produce the same `<hash>  <file>` line shape that the
# combined SHA256SUMS file uses.
if command -v shasum >/dev/null 2>&1; then
    sha_cmd="shasum -a 256"
elif command -v sha256sum >/dev/null 2>&1; then
    sha_cmd="sha256sum"
else
    die "required command not found: shasum or sha256sum"
fi

# ---------------------------------------------------------------------------
# Resolve version
# ---------------------------------------------------------------------------

if [ -z "$version" ]; then
    say "Resolving latest release from GitHub API"
    api_url="https://api.github.com/repos/$REPO/releases/latest"
    # `tag_name` is a stable field on the release object.
    version="$(
        curl -fsSL "$api_url" \
        | sed -n 's/^[[:space:]]*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' \
        | head -n 1
    )"
    if [ -z "$version" ]; then
        die "could not determine latest release tag from $api_url"
    fi
    say "Latest release: $version"
else
    say "Using requested version: $version"
fi

# Strip leading `v` (release-asset filenames embed the bare version).
version_no_v="${version#v}"

# ---------------------------------------------------------------------------
# Download + verify
# ---------------------------------------------------------------------------

tarball_name="tyc-${version_no_v}-${target}.tar.gz"
checksums_name="SHA256SUMS"
base_url="https://github.com/$REPO/releases/download/$version"
tarball_url="$base_url/$tarball_name"
checksums_url="$base_url/$checksums_name"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT INT HUP TERM

say "Downloading $tarball_name"
say "  from $tarball_url"
if ! curl -fSL --proto '=https' --tlsv1.2 -o "$tmpdir/$tarball_name" "$tarball_url"; then
    die "failed to download $tarball_url
Check that the release exists: https://github.com/$REPO/releases/tag/$version"
fi

say "Downloading $checksums_name"
say "  from $checksums_url"
if ! curl -fSL --proto '=https' --tlsv1.2 -o "$tmpdir/$checksums_name" "$checksums_url"; then
    die "failed to download $checksums_url"
fi

say "Verifying SHA-256 checksum"
(
    cd "$tmpdir"
    # Filter the combined SHA256SUMS to just our tarball line to keep
    # output clean. `grep -F` so the `.` in the filename is matched
    # literally rather than as a regex metachar, and the leading two
    # spaces match the `<hash>  <file>` format both `shasum` and
    # `sha256sum` emit.
    grep -F "  $tarball_name" "$checksums_name" > "$tarball_name.sha256" \
        || die "no checksum entry for $tarball_name in $checksums_name"
    # shellcheck disable=SC2086
    $sha_cmd -c "$tarball_name.sha256"
)

say "Extracting archive"
tar -xzf "$tmpdir/$tarball_name" -C "$tmpdir"

extracted_dir="$tmpdir/tyc-${version_no_v}-${target}"
extracted_bin="$extracted_dir/tyc"
if [ ! -x "$extracted_bin" ]; then
    # Some tarballs may not preserve the +x bit — chmod and re-check.
    chmod +x "$extracted_bin" 2>/dev/null || true
fi
if [ ! -f "$extracted_bin" ]; then
    die "expected binary not found in archive: $extracted_bin"
fi

# ---------------------------------------------------------------------------
# Install
# ---------------------------------------------------------------------------

say "Installing to $install_dir"
mkdir -p "$install_dir"

dest="$install_dir/tyc"
# Atomic replace: copy + mv to handle the upgrade-in-place case cleanly.
tmp_dest="$dest.tmp.$$"
cp "$extracted_bin" "$tmp_dest"
chmod 0755 "$tmp_dest"
mv -f "$tmp_dest" "$dest"

if [ "$is_linux" = "0" ]; then
    # Clear the macOS Gatekeeper quarantine attribute so the user is not
    # prompted on first run. Ignore failure: the xattr may not be set if
    # the tarball was downloaded via curl rather than a browser.
    say "Clearing Gatekeeper quarantine attribute (best-effort)"
    xattr -d com.apple.quarantine "$dest" 2>/dev/null || true
fi

# ---------------------------------------------------------------------------
# Smoke-test + PATH hint
# ---------------------------------------------------------------------------

say "Installed $dest"

if "$dest" --version >/dev/null 2>&1; then
    "$dest" --version 2>&1 | sed 's/^/    /'
fi

# PATH check — print exact export lines for bash and zsh if missing.
case ":$PATH:" in
    *":$install_dir:"*)
        say "$install_dir is already on your PATH."
        ;;
    *)
        printf '\n'
        warn "$install_dir is not on your PATH."
        printf '\n'
        printf 'Add it by appending one of the following to your shell rc file:\n'
        printf '\n'
        printf '  # bash (~/.bashrc or ~/.bash_profile)\n'
        printf '  export PATH="%s:$PATH"\n' "$install_dir"
        printf '\n'
        printf '  # zsh (~/.zshrc)\n'
        printf '  export PATH="%s:$PATH"\n' "$install_dir"
        printf '\n'
        printf 'Then restart your shell (or `source` the file).\n'
        printf '\n'
        ;;
esac

say "Done. Run \`tyc --help\` to get started."
