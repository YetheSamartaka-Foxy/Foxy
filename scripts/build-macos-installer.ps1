param(
    [string]$RemoteHost,
    [string]$RemoteRepoPath,
    [string]$OutputDir = "dist"
)

$ErrorActionPreference = "Stop"

function Invoke-Checked {
    param(
        [Parameter(Mandatory = $true)]
        [scriptblock]$Command,
        [Parameter(Mandatory = $true)]
        [string]$Description
    )

    & $Command
    if ($LASTEXITCODE -ne 0) {
        throw "$Description failed with exit code $LASTEXITCODE."
    }
}

function Require-Tool {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Name
    )

    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "Required tool '$Name' was not found in PATH."
    }
}

function Get-AppVersion {
    param(
        [Parameter(Mandatory = $true)]
        [string]$CargoToml
    )

    foreach ($line in Get-Content -Path $CargoToml) {
        if ($line -match '^version\s*=\s*"(.+)"') {
            return $Matches[1]
        }
    }

    throw "Could not read package version from Cargo.toml."
}

function Quote-Sh {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Value
    )

    $singleQuote = [char]39
    return $singleQuote + $Value.Replace($singleQuote, "$singleQuote\${singleQuote}${singleQuote}") + $singleQuote
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$cargoToml = Join-Path $repoRoot "Cargo.toml"
$appVersion = Get-AppVersion -CargoToml $cargoToml
$artifactName = "Foxy-$appVersion-macos-arm64.dmg"
$localOutputDir = Join-Path $repoRoot $OutputDir
$localOutput = Join-Path $localOutputDir $artifactName

$isMacOS = [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
    [System.Runtime.InteropServices.OSPlatform]::OSX
)
$isWindows = [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
    [System.Runtime.InteropServices.OSPlatform]::Windows
)

if ($RemoteHost) {
    if (-not $RemoteRepoPath) {
        throw "RemoteRepoPath is required when RemoteHost is provided."
    }

    Require-Tool "ssh"
    Require-Tool "scp"

    if (-not (Test-Path $localOutputDir)) {
        New-Item -ItemType Directory -Path $localOutputDir | Out-Null
    }

    $quotedRemoteRepoPath = Quote-Sh $RemoteRepoPath
    $remoteOutput = "$RemoteRepoPath/dist/$artifactName"
    $quotedRemoteOutput = Quote-Sh $remoteOutput

    Write-Host "[1/2] Building macOS installer on $RemoteHost..."
    Invoke-Checked `
        -Command { ssh $RemoteHost "cd $quotedRemoteRepoPath && chmod +x scripts/build-macos-installer.sh && ./scripts/build-macos-installer.sh" } `
        -Description "Remote macOS installer build"

    Write-Host "[2/2] Copying macOS dmg to $localOutput..."
    Invoke-Checked `
        -Command { scp "${RemoteHost}:$quotedRemoteOutput" $localOutput } `
        -Description "macOS dmg copy"

    Write-Host ""
    Write-Host "Built installer: $localOutput"
    exit 0
}

if (-not $isMacOS) {
    $hostName = if ($isWindows) { "Windows" } else { "this host" }
    throw "Native macOS dmg builds are not supported on $hostName. Use macOS locally, or run this script with -RemoteHost and -RemoteRepoPath to build on an SSH-accessible Mac."
}

Push-Location $repoRoot
try {
    Require-Tool "bash"
    Invoke-Checked -Command { bash scripts/build-macos-installer.sh } -Description "macOS installer build"
}
finally {
    Pop-Location
}
