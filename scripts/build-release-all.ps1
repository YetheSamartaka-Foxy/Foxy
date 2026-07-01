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

$repoRoot = Split-Path -Parent $PSScriptRoot

Push-Location $repoRoot
try {
    Write-Host "[1/2] Building Windows release..."
    Invoke-Checked -Command { cargo build --release --target x86_64-pc-windows-msvc } -Description "Windows release build"

    Write-Host "Ensuring Linux target is installed..."
    Invoke-Checked -Command { rustup target add x86_64-unknown-linux-gnu | Out-Null } -Description "Linux target installation"

    if ($env:OS -eq "Windows_NT") {
        Write-Host "[2/2] Building Linux release with cross..."
        Require-Tool "cross"
        Invoke-Checked -Command { cross build --release --target x86_64-unknown-linux-gnu } -Description "Linux release build (cross)"
    } else {
        Write-Host "[2/2] Building Linux release..."
        Invoke-Checked -Command { cargo build --release --target x86_64-unknown-linux-gnu } -Description "Linux release build"
    }

    Write-Host ""
    Write-Host "Built artifacts:"
    Write-Host "  target/x86_64-pc-windows-msvc/release/Foxy.exe"
    Write-Host "  target/x86_64-unknown-linux-gnu/release/Foxy"
}
finally {
    Pop-Location
}
