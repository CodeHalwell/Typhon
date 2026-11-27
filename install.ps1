<#
.SYNOPSIS
    Install the Typhon compiler (tyc) on Windows.

.DESCRIPTION
    Downloads a pre-built `tyc.exe` from the latest GitHub Release,
    verifies its SHA-256 checksum, extracts it to a per-user install
    directory, and adds that directory to the user-level PATH.

    When the GitHub CLI (`gh`) is on PATH, the archive's signed build
    provenance is verified as well:

        gh attestation verify <archive> --repo CodeHalwell/Typhon

    An archive that has an attestation which does not verify aborts the
    install. Releases up to and including v1.0.0-beta.7 were published
    before attestations existed, and `gh` may be missing or not logged
    in; in those cases the installer prints a note and carries on with
    the checksum alone. Set TYPHON_REQUIRE_ATTESTATION=1 to make anything
    short of a successful provenance check fatal.

    Default install location:  $env:LOCALAPPDATA\Programs\Typhon
    Default version:           the latest GitHub Release.

.PARAMETER Version
    A release tag to install (e.g. "v0.10.0"). Defaults to the latest
    release resolved via the GitHub API.

.PARAMETER InstallDir
    Where to install tyc.exe. Defaults to $env:LOCALAPPDATA\Programs\Typhon.

.PARAMETER NoPath
    Do not modify the user-level PATH. The install still completes; you
    will need to add the install directory to PATH yourself.

.EXAMPLE
    # Install the latest release
    iwr -useb https://raw.githubusercontent.com/codehalwell/typhon/main/install.ps1 | iex

.EXAMPLE
    # Install a specific version to a custom directory
    .\install.ps1 -Version v0.10.0 -InstallDir C:\Tools\Typhon

.EXAMPLE
    # Install without modifying PATH
    .\install.ps1 -NoPath

.EXAMPLE
    # Insist on verified build provenance (needs an authenticated `gh`)
    $env:TYPHON_REQUIRE_ATTESTATION = '1'; .\install.ps1

.NOTES
    Environment variables equivalent to the parameters:
      TYPHON_VERSION       -> -Version
      TYPHON_INSTALL_DIR   -> -InstallDir

    Environment variable without a parameter:
      TYPHON_REQUIRE_ATTESTATION=1
          Refuse to install unless `gh attestation verify` succeeds for
          the downloaded archive.

    Supported platform: Windows 10 / 11 on x86_64 (AMD64). ARM64 is not
    yet shipped as a pre-built artifact; build from source until it is.
#>

[CmdletBinding()]
param(
    [string]$Version = $env:TYPHON_VERSION,
    [string]$InstallDir = $env:TYPHON_INSTALL_DIR,
    [switch]$NoPath
)

$ErrorActionPreference = 'Stop'

$Repo = 'codehalwell/typhon'
# Canonical owner/name, as recorded in the build-provenance attestations.
$AttestationRepo = 'CodeHalwell/Typhon'
$RequireAttestation = ($env:TYPHON_REQUIRE_ATTESTATION -eq '1')

function Write-Step {
    param([string]$Message)
    Write-Host "==> $Message"
}

function Write-Warn {
    param([string]$Message)
    Write-Warning $Message
}

function Stop-WithError {
    param([string]$Message)
    Write-Error $Message
    exit 1
}

# Runs `gh` with the given arguments and returns its exit code and combined
# output. Windows PowerShell 5.1 turns a native command's redirected stderr
# lines into error records, which `$ErrorActionPreference = 'Stop'` would
# throw on, so the preference is relaxed for the duration of the call (the
# assignment is local to this function).
function Invoke-Gh {
    param([string[]]$GhArgs)
    $ErrorActionPreference = 'Continue'
    $code = -1
    $output = @()
    try {
        $output = @(& gh @GhArgs 2>&1 | ForEach-Object { "$_" })
        $code = $LASTEXITCODE
    } catch {
        $output = @("$_")
        $code = -1
    }
    return @{ Code = $code; Output = $output }
}

# The provenance check could not be completed (as opposed to: it ran and
# failed). Fatal only when TYPHON_REQUIRE_ATTESTATION=1.
function Skip-Attestation {
    param([string]$Reason)
    if ($RequireAttestation) {
        Stop-WithError "$Reason
TYPHON_REQUIRE_ATTESTATION=1 is set, so the install stops here."
    }
    Write-Step "Note: $Reason"
    Write-Step "  Continuing with the SHA-256 checksum only."
    Write-Step "  (Set TYPHON_REQUIRE_ATTESTATION=1 to make this fatal.)"
}

# ---------------------------------------------------------------------------
# Platform detection
# ---------------------------------------------------------------------------

Write-Step "Detecting platform"

$arch = $env:PROCESSOR_ARCHITECTURE
if ($env:PROCESSOR_ARCHITEW6432) {
    $arch = $env:PROCESSOR_ARCHITEW6432
}

switch ($arch) {
    'AMD64' { $target = 'x86_64-pc-windows-msvc' }
    'ARM64' {
        Stop-WithError "Windows ARM64 is not yet shipped as a pre-built artifact.
Build from source: https://github.com/$Repo"
    }
    default {
        Stop-WithError "Unsupported architecture: $arch (expected AMD64 or ARM64)."
    }
}

Write-Step "Platform: Windows / $arch -> target triple $target"

# ---------------------------------------------------------------------------
# Defaults
# ---------------------------------------------------------------------------

if (-not $InstallDir -or $InstallDir.Trim() -eq '') {
    $InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\Typhon'
}

# Use TLS 1.2 explicitly — older PowerShell defaults to SSL3/TLS1.0
# which GitHub long since dropped.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

# `-UseBasicParsing` is only meaningful on Windows PowerShell 5.1 — on
# PowerShell 6+ basic parsing is the only mode and the flag is a no-op
# (still accepted, but flagged as obsolete). Build a splat once so each
# call site stays clean and we don't pass a useless flag on modern PS.
$WebRequestExtra = @{}
if ($PSVersionTable.PSVersion.Major -lt 6) {
    $WebRequestExtra['UseBasicParsing'] = $true
}

# ---------------------------------------------------------------------------
# Resolve version
# ---------------------------------------------------------------------------

if (-not $Version -or $Version.Trim() -eq '') {
    Write-Step "Resolving latest release from GitHub API"
    # /releases/latest excludes pre-releases, and every alpha/beta tag is
    # (correctly) published as a pre-release — so it would resolve to a
    # stale build during the pre-1.0 line. The release list is ordered
    # newest-first and includes pre-releases. Unauthenticated requests
    # never receive draft releases, but filter them defensively anyway.
    $apiUrl = "https://api.github.com/repos/$Repo/releases?per_page=5"
    try {
        $releases = Invoke-RestMethod -Uri $apiUrl -Headers @{
            'User-Agent' = 'typhon-install-ps1'
        } @WebRequestExtra
    } catch {
        Stop-WithError "Could not query GitHub Releases: $_"
    }
    $release = @($releases) | Where-Object { -not $_.draft } | Select-Object -First 1
    $Version = $release.tag_name
    if (-not $Version) {
        Stop-WithError "Could not determine latest release tag from $apiUrl"
    }
    Write-Step "Latest release: $Version"
} else {
    Write-Step "Using requested version: $Version"
}

$VersionNoV = $Version.TrimStart('v')

# ---------------------------------------------------------------------------
# Download + verify
# ---------------------------------------------------------------------------

$ZipName       = "tyc-$VersionNoV-$target.zip"
$ChecksumsName = 'SHA256SUMS'
$BaseUrl       = "https://github.com/$Repo/releases/download/$Version"
$ZipUrl        = "$BaseUrl/$ZipName"
$ChecksumsUrl  = "$BaseUrl/$ChecksumsName"

$TmpDir = Join-Path ([IO.Path]::GetTempPath()) ("typhon-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $TmpDir | Out-Null

try {
    $ZipPath       = Join-Path $TmpDir $ZipName
    $ChecksumsPath = Join-Path $TmpDir $ChecksumsName

    Write-Step "Downloading $ZipName"
    Write-Step "  from $ZipUrl"
    try {
        Invoke-WebRequest -Uri $ZipUrl -OutFile $ZipPath @WebRequestExtra
    } catch {
        Stop-WithError "Failed to download ${ZipUrl}: $_
Check that the release exists: https://github.com/$Repo/releases/tag/$Version"
    }

    Write-Step "Downloading $ChecksumsName"
    Write-Step "  from $ChecksumsUrl"
    try {
        Invoke-WebRequest -Uri $ChecksumsUrl -OutFile $ChecksumsPath @WebRequestExtra
    } catch {
        Stop-WithError "Failed to download ${ChecksumsUrl}: $_"
    }

    Write-Step "Verifying SHA-256 checksum"
    # `sha256sum` writes `<hash>  <file>` in text mode (two spaces) and
    # `<hash> *<file>` in binary mode. Our own release workflow emits the
    # text form, but accept both shapes so a hand-rolled SHA256SUMS
    # (`sha256sum -b`) still verifies correctly.
    $expectedLine = Get-Content -LiteralPath $ChecksumsPath | Where-Object { $_ -match "[\s\*]$([regex]::Escape($ZipName))\s*$" } | Select-Object -First 1
    if (-not $expectedLine) {
        Stop-WithError "No checksum entry for $ZipName in $ChecksumsName"
    }
    $expectedHash = ($expectedLine -split '\s+', 2)[0].ToLowerInvariant()
    $actualHash   = (Get-FileHash -Path $ZipPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($expectedHash -ne $actualHash) {
        Stop-WithError "SHA-256 mismatch for ${ZipName}:
  expected $expectedHash
  actual   $actualHash"
    }
    Write-Step "Checksum OK ($actualHash)"

    # -----------------------------------------------------------------------
    # Build provenance (optional unless TYPHON_REQUIRE_ATTESTATION=1)
    # -----------------------------------------------------------------------
    #
    # The checksum above only proves the archive matches the SHA256SUMS file
    # that sits next to it in the same release. The attestation is signed by
    # the release workflow itself, so it ties the archive to that workflow
    # run and source commit independently of the release's own files.

    if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
        Skip-Attestation "build provenance not checked: the GitHub CLI (gh) is not installed."
    } elseif ((Invoke-Gh @('attestation', 'verify', '--help')).Code -ne 0) {
        Skip-Attestation "build provenance not checked: this gh has no 'attestation verify' command (too old)."
    } else {
        Write-Step "Verifying build provenance (gh attestation verify)"
        $verify = Invoke-Gh @('attestation', 'verify', $ZipPath, '--repo', $AttestationRepo)
        if ($verify.Code -eq 0) {
            Write-Step "Build provenance OK (built by $AttestationRepo)"
        } else {
            foreach ($line in $verify.Output) { Write-Host "    gh: $line" }
            # Verification did not succeed. Tell "this archive has an
            # attestation and it does not verify" (fatal) apart from "there
            # is nothing to verify against" (old release, or gh cannot reach
            # or log in to the API). gh's wording for these differs between
            # versions, so ask the API directly how many attestations exist
            # for this exact digest.
            $probe = Invoke-Gh @('api', "repos/$AttestationRepo/attestations/sha256:$actualHash", '--jq', '.attestations | length')
            $attestationCount = 0
            $parsed = $false
            if ($probe.Code -eq 0 -and $probe.Output.Count -gt 0) {
                $parsed = [int]::TryParse(("$($probe.Output[-1])").Trim(), [ref]$attestationCount)
            }
            if ($probe.Code -eq 0 -and $parsed -and $attestationCount -gt 0) {
                Stop-WithError "Build provenance verification FAILED for $ZipName.
The release has an attestation for this archive, but it does not verify.
Refusing to install. Report this at https://github.com/$Repo/security"
            }
            Skip-Attestation "build provenance not verified: no attestation for $ZipName could be fetched (releases up to v1.0.0-beta.7 have none; or gh is not logged in)."
        }
    }

    Write-Step "Extracting archive"
    $ExtractDir = Join-Path $TmpDir 'extract'
    Expand-Archive -Path $ZipPath -DestinationPath $ExtractDir -Force

    # A release archive holds a directory with tyc.exe and a few text files.
    # A symbolic link or junction has no business being there and could
    # redirect the copy below.
    $links = @(Get-ChildItem -LiteralPath $ExtractDir -Recurse -Force |
        Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint })
    if ($links.Count -gt 0) {
        Stop-WithError "Archive $ZipName contains links (for example $($links[0].FullName)); refusing to install."
    }

    # The archive contains a top-level tyc-<version>-<target>\ directory
    # holding tyc.exe and the README / LICENSE.
    $InnerDir = Join-Path $ExtractDir "tyc-$VersionNoV-$target"
    $ExtractedExe = Join-Path $InnerDir 'tyc.exe'
    if (-not (Test-Path -LiteralPath $ExtractedExe)) {
        # Fallback: look for tyc.exe anywhere in the extracted tree.
        $found = Get-ChildItem -Path $ExtractDir -Recurse -Filter 'tyc.exe' | Select-Object -First 1
        if ($found) {
            $ExtractedExe = $found.FullName
        } else {
            Stop-WithError "Expected tyc.exe not found in archive at $InnerDir"
        }
    }

    # -----------------------------------------------------------------------
    # Install
    # -----------------------------------------------------------------------

    Write-Step "Installing to $InstallDir"
    if (-not (Test-Path -LiteralPath $InstallDir)) {
        New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    }

    $DestExe = Join-Path $InstallDir 'tyc.exe'

    # If a previous install is currently running, the move will fail with
    # a sharing violation. Try a slightly delayed retry, but surface a
    # clear error if that doesn't help — the user knows whether tyc is
    # running better than we do.
    $maxAttempts = 3
    for ($attempt = 1; $attempt -le $maxAttempts; $attempt++) {
        try {
            Copy-Item -LiteralPath $ExtractedExe -Destination $DestExe -Force
            break
        } catch {
            if ($attempt -eq $maxAttempts) {
                Stop-WithError "Failed to install ${DestExe}: $_
If tyc.exe is currently running, close all sessions and retry."
            }
            Start-Sleep -Seconds 1
        }
    }

    # -----------------------------------------------------------------------
    # PATH setup
    # -----------------------------------------------------------------------

    if (-not $NoPath) {
        $userPath = [Environment]::GetEnvironmentVariable('PATH', 'User')
        $userPathParts = if ($userPath) { $userPath -split ';' } else { @() }
        $hasDir = $false
        foreach ($p in $userPathParts) {
            if ($p -and ($p.TrimEnd('\') -ieq $InstallDir.TrimEnd('\'))) {
                $hasDir = $true
                break
            }
        }
        if ($hasDir) {
            Write-Step "$InstallDir is already on your user PATH."
        } else {
            Write-Step "Adding $InstallDir to your user PATH"
            # TrimEnd(';') so we don't end up with `a;b;;C:\…` when the
            # existing user PATH already has a trailing semicolon (common
            # leftover from previous tooling).
            $newUserPath = if ($userPath) { "$($userPath.TrimEnd(';'));$InstallDir" } else { $InstallDir }
            [Environment]::SetEnvironmentVariable('PATH', $newUserPath, 'User')
            # Also expose it in the current session so the smoke test
            # below (and any follow-up command the user runs in this
            # window) picks it up immediately.
            $env:PATH = "$env:PATH;$InstallDir"
            Write-Warn "Open a new terminal for the PATH change to take effect in other shells."
        }
    } else {
        Write-Step "Skipping PATH modification (-NoPath)."
    }

    # -----------------------------------------------------------------------
    # Smoke-test
    # -----------------------------------------------------------------------

    Write-Step "Installed $DestExe"
    try {
        & $DestExe --version | ForEach-Object { "    $_" } | Write-Host
    } catch {
        Write-Warn "tyc --version failed: $_"
    }

    Write-Step "Done. Run ``tyc --help`` to get started."
}
finally {
    if (Test-Path -LiteralPath $TmpDir) {
        Remove-Item -LiteralPath $TmpDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}
# typhon-installer: end
