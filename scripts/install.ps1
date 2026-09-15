#!/usr/bin/env pwsh
# Install one pinned, signed supra release on Windows (PowerShell/cmd).
#
#   $env:SUPRA_VERSION="vX.Y.Z"
#   $env:SUPRA_PUBKEY="<published minisign public key>"
#   irm .../supra-harness/vX.Y.Z/scripts/install.ps1 | iex
#
# or:  .\install.ps1 -Version v0.3.0 -PubKey '<minisign public key>'
#
# Downloads the per-target .tar.gz plus its manifest, minisign signature, and
# SHA256SUMS, verifies the archive's SHA-256 (always) and, when a `minisign`
# binary is on PATH, the manifest signature too, checks that the manifest
# binds this release/target, extracts the single executable member, and
# installs it to a user-writable location (no admin) added to the user PATH.

[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidatePattern('^v\d+\.\d+\.\d+(.|-.+)?$')][string]$Version,
    [Parameter(Mandatory)][string]$PubKey,
    [string]$Prefix = (Join-Path $env:LOCALAPPDATA "supra"),
    [string]$Repo = "Anonnymous0071/supra-harness"
)
$ErrorActionPreference = "Stop"
$target = "x86_64-pc-windows-msvc"

function Fail([string]$msg) { Write-Error "install: $msg"; exit 1 }

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("supra-install-" + [Guid]::NewGuid().ToString("n"))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $base = "supra-$target"
    $archive = "$base.tar.gz"
    $manifest = "$base.manifest.json"
    $signature = "$manifest.minisig"
    $sums = "SHA256SUMS"
    $url = "https://github.com/$Repo/releases/download/$Version"
    foreach ($file in @($archive, $manifest, $signature, $sums)) {
        Write-Host "install: downloading $file"
        Invoke-WebRequest -Uri "$url/$file" -OutFile (Join-Path $tmp $file) -UseBasicParsing
    }

    # 1. Archive integrity against the release-published SHA256SUMS (built-in).
    $sumsText = Get-Content (Join-Path $tmp $sums) -Raw
    $want = ($sumsText -split [Environment]::NewLine | Where-Object { $_ -match [regex]::Escape($archive) } | Select-Object -First 1)
    if (-not $want) { Fail "archive $archive is missing from SHA256SUMS" }
    $wantHash = ($want -split '\s+' | Select-Object -First 1).ToLowerInvariant()
    $gotHash = (Get-FileHash (Join-Path $tmp $archive) -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($wantHash -ne $gotHash) { Fail "archive SHA-256 does not match the release's SHA256SUMS" }

    # 2. If a minisign binary is present, verify the manifest signature.
    $ms = Get-Command minisign -ErrorAction SilentlyContinue
    if ($ms) {
        Set-Content -Path (Join-Path $tmp "supra.pub") -Value $PubKey -NoNewline
        & minisign -Vm (Join-Path $tmp $manifest) -p (Join-Path $tmp "supra.pub") -x (Join-Path $tmp $signature) 2>&1 | Out-Null
        if ($LASTEXITCODE -ne 0) { Fail "manifest signature verification FAILED" }
        Write-Host "install: manifest signature verified"
    }
    else {
        Write-Warning "install: 'minisign' not on PATH; signature not verified (SHA-256 only)"
    }

    # 3. The manifest must bind this release/target and the exact archive.
    $raw = [System.IO.File]::ReadAllBytes((Join-Path $tmp $manifest))
    $text = [System.Text.Encoding]::UTF8.GetString($raw)
    $data = $text | ConvertFrom-Json
    $expectedMember = "supra-$target/supra.exe"
    $ver = $Version.TrimStart('v')
    if ("$($data.version)" -ne $ver) { Fail "manifest version does not match $Version" }
    if ("$($data.target)" -ne $target) { Fail "manifest target is not $target" }
    if ("$($data.archive)" -ne $archive) { Fail "manifest archive name mismatch" }
    $archiveBytes = [System.IO.File]::ReadAllBytes((Join-Path $tmp $archive))
    if ([int64]$data.archive_size -ne $archiveBytes.Length) { Fail "archive size binding mismatch" }
    $archiveSha = ([System.Convert]::ToHexString((New-Object System.Security.Cryptography.SHA256).ComputeHash($archiveBytes) -join "").ToLowerInvariant())
    if ($data.archive_sha256.ToLowerInvariant() -ne $archiveSha) { Fail "archive digest binding mismatch" }

    # 4. The archive holds exactly one executable member; extract and bind it.
    $work = Join-Path $tmp "x"
    New-Item -ItemType Directory -Path $work | Out-Null
    tar -xzf (Join-Path $tmp $archive) -C $work
    $member = Get-ChildItem -Path $work -Recurse -File
    if (($member | Measure-Object).Count -ne 1) { Fail "archive does not contain a single member" }
    $exe = $member[0].FullName
    if ((Split-Path $exe -Leaf) -ne "supra.exe") { Fail "expected member supra.exe" }
    $exeBytes = [System.IO.File]::ReadAllBytes($exe)
    if ($data.executable.size -ne $exeBytes.Length) { Fail "executable size binding mismatch" }
    $exeSha = ([System.Convert]::ToHexString((New-Object System.Security.Cryptography.SHA256).ComputeHash($exeBytes) -join "").ToLowerInvariant())
    if ($data.executable.sha256.ToLowerInvariant() -ne $exeSha) { Fail "executable digest binding mismatch" }

    # 5. Install to a user-writable prefix and put it on the user PATH.
    New-Item -ItemType Directory -Path $Prefix -Force | Out-Null
    $dest = Join-Path $Prefix "supra.exe"
    Copy-Item $exe $dest -Force
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($userPath -notlike "*$Prefix*") {
        [Environment]::SetEnvironmentVariable("Path", "$Prefix;$userPath", "User")
        Write-Host "install: added $Prefix to the user PATH (open a new shell to pick it up)"
    }
    Write-Host "install: supra ${Version} -> $dest"
    & $dest --version
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
