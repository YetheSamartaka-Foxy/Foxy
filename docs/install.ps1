# Installs or updates Foxy. Run it in PowerShell:
#
#   irm https://yethesamartaka-foxy.github.io/Foxy/install.ps1 | iex
#
# It downloads the newest Foxy setup from GitHub releases, checks its SHA-256, and runs that
# installer. A new install goes to %LOCALAPPDATA%\Programs\Foxy, with a Start menu entry and an
# entry in Settings > Apps. No administrator permission is needed for that current-user install.
# When Foxy is already installed for all users, the same command updates that copy and Windows
# asks for administrator permission.
#
# Saved repositories, settings, and databases stay in %APPDATA%\Foxy. Downloaded mods stay in
# the folders you chose. Run the command again to update. Remove Foxy in Settings > Apps.
#
# FOXY_INSTALL_REPO overrides the GitHub repository (owner/name). Point it only at a repository
# you trust, because this script installs what that repository publishes.
# FOXY_INSTALL_DRY_RUN=1 prints the chosen download and stops before installing.
#
# Everything sits in a function so that piping into iex never runs `exit` in your window.

function Install-Foxy {
    $ErrorActionPreference = 'Stop'
    # Windows PowerShell 5.1 may still default to TLS 1.0, which GitHub refuses.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $repo = if ($env:FOXY_INSTALL_REPO) { $env:FOXY_INSTALL_REPO.Trim() } else { 'YetheSamartaka-Foxy/Foxy' }
    if ($repo -notmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$') {
        Write-Host "FOXY_INSTALL_REPO must look like owner/name. Nothing was installed." -ForegroundColor Red
        return
    }

    $headers = @{
        'User-Agent' = 'Foxy-installer'
        'Accept'     = 'application/vnd.github+json'
    }

    # The native architecture: PROCESSOR_ARCHITECTURE of this process says AMD64 when x64
    # PowerShell runs emulated on an ARM PC, the machine-wide value does not.
    $native = (Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment').PROCESSOR_ARCHITECTURE
    if ($native -ne 'AMD64' -and $native -ne 'ARM64') {
        Write-Host "Foxy needs 64-bit Windows (x64, including ARM PCs). This PC reports '$native'." -ForegroundColor Red
        return
    }

    Write-Host "Looking up the newest Foxy release..."
    try {
        $release = Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest" -Headers $headers
    } catch {
        Write-Host "Could not look up the newest Foxy release. Nothing was installed." -ForegroundColor Red
        return
    }
    if (-not $release.tag_name -or -not $release.assets) {
        Write-Host "Could not look up the newest Foxy release. Nothing was installed." -ForegroundColor Red
        return
    }

    $version = $release.tag_name -replace '^v', ''
    $asset = $release.assets | Where-Object { $_.name -eq "Foxy-$version-setup.exe" } | Select-Object -First 1
    if (-not $asset) {
        $asset = $release.assets | Where-Object { $_.name -like 'Foxy-*-setup.exe' -and $_.name -notlike '*cli*' } | Select-Object -First 1
    }
    if (-not $asset) {
        Write-Host "Release $($release.tag_name) has no Foxy Windows setup." -ForegroundColor Red
        return
    }

    $expected = Get-FoxyExpectedSha256 $release $asset $headers
    if (-not $expected) {
        return
    }

    $uninstallId = '{E8A3F5D2-7C41-4B9E-A6D1-3F5E8C2A9B70}_is1'
    $userKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$uninstallId"
    $machineKey = "HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$uninstallId"
    $machineKey32 = "HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\$uninstallId"
    $userInstall = Get-FoxyUninstallRecord $userKey
    $machineInstall = Get-FoxyUninstallRecord $machineKey
    if (-not $machineInstall) {
        $machineInstall = Get-FoxyUninstallRecord $machineKey32
    }
    $existed = [bool]($userInstall -or $machineInstall)
    $elevate = [bool]($machineInstall -and -not $userInstall)

    if ($env:FOXY_INSTALL_DRY_RUN -eq '1') {
        $scope = if ($elevate) { 'machine' } else { 'user' }
        Write-Host "dry-run version=$version asset=$($asset.name) sha256=$expected scope=$scope"
        return
    }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("Foxy-install-" + [Guid]::NewGuid().ToString('n'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    $progress = $ProgressPreference
    $installer = Join-Path $tmp $asset.name
    try {
        # The progress bar makes Invoke-WebRequest many times slower in Windows PowerShell 5.1.
        $ProgressPreference = 'SilentlyContinue'
        Write-Host "Downloading Foxy $version..."
        Invoke-WebRequest $asset.browser_download_url -OutFile $installer -UseBasicParsing -Headers $headers
        $actual = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $expected) {
            Write-Host "The download is damaged (SHA-256 does not match). Nothing was installed." -ForegroundColor Red
            return
        }

        $arguments = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/CLOSEAPPLICATIONS', '/MERGETASKS=!deleteuserdata')
        if ($elevate) {
            Write-Host "Foxy is installed for all users. Windows will ask for administrator permission to update it."
            $arguments = @('/ALLUSERS') + $arguments
        } else {
            if ($machineInstall -and $userInstall) {
                Write-Host "Updating the current-user installation. The all-users installation is left as it is."
            }
            $arguments = @('/CURRENTUSER') + $arguments
        }
        Write-Host "A running Foxy is closed and then started again. Saved repositories, settings, and databases are kept."
        $code = Invoke-FoxySetup $installer $arguments $elevate
        if ($code -ne 0) {
            Write-Host "The installer did not finish (exit $code). Saved Foxy data was left in place." -ForegroundColor Red
            return
        }
    } catch {
        Write-Host $_.Exception.Message -ForegroundColor Red
        return
    } finally {
        $ProgressPreference = $progress
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }

    if ($existed) {
        Write-Host "Foxy is updated to $version." -ForegroundColor Green
    } else {
        Write-Host "Foxy $version is installed. Find it in the Start menu as Foxy." -ForegroundColor Green
    }
    Write-Host "Run this command again to update. Remove Foxy in Settings > Apps."
}

function Get-FoxyUninstallRecord($path) {
    if (-not (Test-Path -LiteralPath $path)) {
        return $null
    }
    return Get-ItemProperty -LiteralPath $path
}

function ConvertTo-FoxyText($value) {
    if ($value -is [byte[]]) {
        return [Text.Encoding]::UTF8.GetString($value)
    }
    return [string]$value
}

function Get-FoxyExpectedSha256($release, $asset, $headers) {
    $digest = $null
    if ($asset.digest -match '^sha256:([0-9a-fA-F]{64})$') {
        $digest = $Matches[1].ToLowerInvariant()
    }

    $fromFile = $null
    try {
        $sidecar = $release.assets | Where-Object { $_.name -eq ($asset.name + '.sha256') } | Select-Object -First 1
        if ($sidecar) {
            $text = ConvertTo-FoxyText (Invoke-WebRequest $sidecar.browser_download_url -UseBasicParsing -Headers $headers).Content
            $token = (($text.Trim() -split '\s+') | Where-Object { $_ } | Select-Object -First 1)
            if ($token -match '^[0-9a-fA-F]{64}$') {
                $fromFile = $token.ToLowerInvariant()
            }
        } else {
            $sums = $release.assets | Where-Object { $_.name -eq 'SHA256SUMS.txt' } | Select-Object -First 1
            if ($sums) {
                $text = ConvertTo-FoxyText (Invoke-WebRequest $sums.browser_download_url -UseBasicParsing -Headers $headers).Content
                $hashes = @()
                foreach ($line in ($text -split '\r?\n')) {
                    $parts = @(($line.Trim() -split '\s+') | Where-Object { $_ })
                    if ($parts.Length -ge 2 -and $parts[1].TrimStart('*') -eq $asset.name -and $parts[0] -match '^[0-9a-fA-F]{64}$') {
                        $hashes += $parts[0].ToLowerInvariant()
                    }
                }
                $unique = @($hashes | Select-Object -Unique)
                if ($unique.Length -eq 1) {
                    $fromFile = [string]$unique[0]
                }
            }
        }
    } catch {
        Write-Host "Could not check the published SHA-256. Nothing was installed." -ForegroundColor Red
        return $null
    }

    if ($fromFile -and $digest -and $fromFile -ne $digest) {
        Write-Host "The published checksum for $($asset.name) does not match the release digest. Nothing was installed." -ForegroundColor Red
        return $null
    }
    $expected = if ($fromFile) { $fromFile } else { $digest }
    if (-not $expected -or $expected -notmatch '^[0-9a-f]{64}$') {
        Write-Host "Release $($release.tag_name) has no SHA-256 for $($asset.name). Nothing was installed." -ForegroundColor Red
        return $null
    }
    return $expected
}

function Invoke-FoxySetup($path, $arguments, $elevate) {
    if ($elevate) {
        $proc = Start-Process -FilePath $path -ArgumentList $arguments -Verb RunAs -PassThru
    } else {
        $proc = Start-Process -FilePath $path -ArgumentList $arguments -PassThru
    }
    # Inno can leave a short gap while it starts its 64-bit copy. Wait until the setup
    # has been gone for two seconds so that copy is included, then the temp file can go.
    $deadline = (Get-Date).AddMinutes(20)
    $quietSince = $null
    do {
        $running = @(Get-Process -ErrorAction SilentlyContinue | Where-Object {
            $_.Id -eq $proc.Id -or $(try { $_.Path -eq $path } catch { $false })
        })
        if ($running.Length -eq 0) {
            if (-not $quietSince) { $quietSince = Get-Date }
            if (((Get-Date) - $quietSince).TotalSeconds -ge 2) { break }
        } else {
            $quietSince = $null
        }
        Start-Sleep -Milliseconds 400
    } while ((Get-Date) -lt $deadline)
    if ($running.Length -gt 0) {
        throw "The installer is still running."
    }
    $proc.Refresh()
    if ($null -eq $proc.ExitCode) {
        return 1
    }
    return [int]$proc.ExitCode
}

Install-Foxy
