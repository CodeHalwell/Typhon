#!/bin/sh
# install.sh — macOS / Linux installer for the Typhon compiler (`tyc`).
#
# Usage:
#   curl --proto '=https' --tlsv1.2 -fsSL https://raw.githubusercontent.com/codehalwell/typhon/main/install.sh | sh
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
# Env vars (no flag equivalent):
#   TYPHON_REQUIRE_ATTESTATION=1   Refuse to install unless the archive's
#                                  build provenance verifies (needs `gh`)
#
# Verification: the archive's SHA-256 is always checked against the release's
# SHA256SUMS. When the GitHub CLI (`gh`) is on PATH, the archive's signed build
# provenance is checked as well (`gh attestation verify`) — see --help.
#
# This script is POSIX-sh and verbose; it prints what it's about to do
# before each step.

set -eu

# The whole installer runs inside `main`, called on the very last line. When
# this script is piped into `sh` and the download is cut short, the shell
# reaches end-of-input before `main` is ever invoked, so a truncated script
# runs nothing instead of running half of a command.
main() {

REPO="codehalwell/typhon"
# Canonical owner/name, as recorded in the build-provenance attestations.
ATTESTATION_REPO="CodeHalwell/Typhon"
DEFAULT_DIR="$HOME/.local/bin"

# ---------------------------------------------------------------------------
# Defaults / arg parsing
# ---------------------------------------------------------------------------

version="${TYPHON_VERSION:-}"
install_dir="${TYPHON_INSTALL_DIR:-$DEFAULT_DIR}"
require_attestation="${TYPHON_REQUIRE_ATTESTATION:-0}"

print_help() {
    cat <<'EOF'
install.sh — install the Typhon compiler (`tyc`) on macOS or Linux.

Usage:
  curl --proto '=https' --tlsv1.2 -fsSL https://raw.githubusercontent.com/codehalwell/typhon/main/install.sh | sh
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
  TYPHON_REQUIRE_ATTESTATION
                       Set to 1 to refuse the install unless the archive's
                       build provenance verifies (see below). Default: 0.

Verification:
  The archive's SHA-256 is always checked against the release's SHA256SUMS
  file, and the archive must contain only regular files and directories.

  When the GitHub CLI (`gh`) is on PATH, the installer also verifies the
  archive's signed build provenance:

    gh attestation verify <archive> --repo CodeHalwell/Typhon

  An archive that has an attestation which does not verify aborts the
  install. Releases up to and including v1.0.0-beta.7 were published before
  attestations existed, and `gh` may be missing or not logged in; in those
  cases the installer prints a note and carries on with the checksum alone.
  Set TYPHON_REQUIRE_ATTESTATION=1 to make anything short of a successful
  provenance check fatal.

Examples:
  # Install latest release
  curl --proto '=https' --tlsv1.2 -fsSL https://raw.githubusercontent.com/codehalwell/typhon/main/install.sh | sh

  # Install a specific version into a custom directory
  TYPHON_VERSION=v0.10.0 TYPHON_INSTALL_DIR=/opt/typhon/bin sh install.sh

  # Insist on verified build provenance (needs an authenticated `gh`)
  TYPHON_REQUIRE_ATTESTATION=1 sh install.sh
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
    # /releases/latest excludes pre-releases, and every alpha/beta tag is
    # (correctly) published as a pre-release — so it would resolve to a
    # stale build during the pre-1.0 line. The release list is ordered
    # newest-first and includes pre-releases. Unauthenticated requests
    # never receive draft releases, but filter them defensively anyway:
    # within each release object `tag_name` precedes `draft`, so remember
    # the most recent tag and emit it at the first `"draft": false`.
    api_url="https://api.github.com/repos/$REPO/releases?per_page=5"
    version="$(
        curl -fsSL --proto '=https' --tlsv1.2 "$api_url" \
        | awk -F'"' '
            /"tag_name":/          { tag = $4 }
            /"draft":[[:space:]]*false/ { if (tag != "") { print tag; exit } }
          '
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
# Set once the staged copy of the binary exists in the install directory;
# cleared again after it has been moved into place.
tmp_dest=""
cleanup() {
    rm -rf "$tmpdir"
    if [ -n "$tmp_dest" ]; then
        rm -f "$tmp_dest"
    fi
}
# Clean up on every exit. A signal handler that only cleaned up would return
# and let the script carry on without its temporary directory, so each signal
# exits instead (128 + signal number), which runs the EXIT trap.
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

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
    $sha_cmd -c "$tarball_name.sha256" \
        || die "SHA-256 checksum mismatch for $tarball_name; refusing to install"
)

# ---------------------------------------------------------------------------
# Build provenance (optional unless TYPHON_REQUIRE_ATTESTATION=1)
# ---------------------------------------------------------------------------
#
# The checksum above only proves the archive matches the SHA256SUMS file that
# sits next to it in the same release. The attestation is signed by the
# release workflow itself, so it ties the archive to that workflow run and
# source commit independently of the release's own files.

attestation_skipped() {
    # $1: why the provenance check did not happen.
    if [ "$require_attestation" = "1" ]; then
        die "$1
TYPHON_REQUIRE_ATTESTATION=1 is set, so the install stops here."
    fi
    say "Note: $1"
    say "  Continuing with the SHA-256 checksum only."
    say "  (Set TYPHON_REQUIRE_ATTESTATION=1 to make this fatal.)"
}

if ! command -v gh >/dev/null 2>&1; then
    attestation_skipped "build provenance not checked: the GitHub CLI (gh) is not installed."
elif ! gh attestation verify --help </dev/null >/dev/null 2>&1; then
    attestation_skipped "build provenance not checked: this gh has no 'attestation verify' command (too old)."
else
    say "Verifying build provenance (gh attestation verify)"
    if gh attestation verify "$tmpdir/$tarball_name" --repo "$ATTESTATION_REPO" \
        </dev/null > "$tmpdir/attestation.log" 2>&1; then
        say "Build provenance OK (built by $ATTESTATION_REPO)"
    else
        sed 's/^/    gh: /' "$tmpdir/attestation.log" >&2
        # Verification did not succeed. Tell "this archive has an attestation
        # and it does not verify" (fatal) apart from "there is nothing to
        # verify against" (old release, or gh cannot reach or log in to the
        # API). gh's wording for these differs between versions, so ask the
        # API directly how many attestations exist for this exact digest.
        # shellcheck disable=SC2086
        digest="$($sha_cmd "$tmpdir/$tarball_name" | awk '{ print $1 }')"
        attestation_count="$(
            gh api "repos/$ATTESTATION_REPO/attestations/sha256:$digest" \
                --jq '.attestations | length' </dev/null 2>/dev/null
        )" || attestation_count=""
        case "$attestation_count" in
            ""|0)
                attestation_skipped "build provenance not verified: no attestation for $tarball_name could be fetched (releases up to v1.0.0-beta.7 have none; or gh is not logged in)."
                ;;
            *[!0-9]*)
                attestation_skipped "build provenance not verified: unexpected answer from the attestation API."
                ;;
            *)
                die "build provenance verification FAILED for $tarball_name.
The release has an attestation for this archive, but it does not verify.
Refusing to install. Report this at https://github.com/$REPO/security"
                ;;
        esac
    fi
fi

# ---------------------------------------------------------------------------
# Extract
# ---------------------------------------------------------------------------

say "Checking archive contents"
# Refuse an archive whose member names are absolute or climb out of the
# extraction directory. Checked on the listing, before anything is written.
tar -tzf "$tmpdir/$tarball_name" > "$tmpdir/listing" \
    || die "could not list the contents of $tarball_name"
if [ ! -s "$tmpdir/listing" ]; then
    die "archive $tarball_name is empty"
fi
if grep -E '^/|(^|/)\.\.(/|$)' "$tmpdir/listing" >&2; then
    die "archive $tarball_name contains an absolute path or a '..' component; refusing to extract"
fi

say "Extracting archive"
extract_dir="$tmpdir/extract"
mkdir "$extract_dir"
# Never restore the owner recorded in the archive (tar does that by default
# when run as root). GNU tar and bsdtar accept --no-same-owner; older BusyBox
# tar does not, so probe with a throwaway archive and fall back to plain -x.
tar_owner_flag=""
mkdir "$tmpdir/probe" "$tmpdir/probe-out"
: > "$tmpdir/probe/f"
if tar -cf "$tmpdir/probe.tar" -C "$tmpdir/probe" f >/dev/null 2>&1 \
    && tar --no-same-owner -xf "$tmpdir/probe.tar" -C "$tmpdir/probe-out" >/dev/null 2>&1 \
    && [ -f "$tmpdir/probe-out/f" ]; then
    tar_owner_flag="--no-same-owner"
fi
# shellcheck disable=SC2086
tar $tar_owner_flag -xzf "$tmpdir/$tarball_name" -C "$extract_dir"

# A release archive holds a directory with the binary and a few text files.
# Anything else — a symlink above all, but also a device node or FIFO — has
# no business being there and could redirect the copy below.
unexpected="$(find "$extract_dir" ! -type f ! -type d | head -n 5)"
if [ -n "$unexpected" ]; then
    printf '%s\n' "$unexpected" | sed 's/^/    /' >&2
    die "archive $tarball_name contains entries that are not regular files or directories (symlinks?); refusing to install"
fi

extracted_dir="$extract_dir/tyc-${version_no_v}-${target}"
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
# Atomic replace: copy + mv to handle the upgrade-in-place case cleanly. The
# staging file is created by mktemp (unpredictable name, created exclusively,
# mode 0600) in the install directory itself, so the final `mv` is a rename
# within one filesystem. The EXIT trap removes it if anything below fails.
tmp_dest="$(mktemp "$install_dir/.tyc.XXXXXX")" \
    || die "could not create a temporary file in $install_dir"
cp "$extracted_bin" "$tmp_dest"
chmod 0755 "$tmp_dest"
mv -f "$tmp_dest" "$dest"
tmp_dest=""

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

}

main "$@"
# typhon-installer: end
