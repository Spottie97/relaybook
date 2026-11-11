# Install Relaybook on Windows. No admin rights and no execution-policy change.
#
#   irm https://raw.githubusercontent.com/Spottie97/relaybook/main/install.ps1 | iex
#
# Flags do not reach a script that is piped into iex. Pass them by calling the
# downloaded text as a scriptblock:
#
#   & ([scriptblock]::Create((irm https://raw.githubusercontent.com/Spottie97/relaybook/main/install.ps1))) -Upgrade -Yes
#
# Or set environment variables before the pipe. Use 1 for each flag:
# RELAYBOOK_UPGRADE, RELAYBOOK_UNINSTALL, RELAYBOOK_NO_GRAPHIFY, RELAYBOOK_WITH_RTK, RELAYBOOK_YES.
# RELAYBOOK_INSTALL_DIR and RELAYBOOK_RELEASE_BASE override the install folder and the download prefix.

[CmdletBinding()]
param(
    [switch]$NoGraphify,
    [switch]$Upgrade,
    [switch]$Uninstall,
    [switch]$WithRtk,
    [switch]$Yes
)

$ErrorActionPreference = "Stop"

function Test-EnvFlag {
    param([string]$Name)
    $value = [Environment]::GetEnvironmentVariable($Name)
    return $value -eq "1" -or $value -eq "true"
}

$DoUpgrade = [bool]$Upgrade -or (Test-EnvFlag "RELAYBOOK_UPGRADE")
$DoUninstall = [bool]$Uninstall -or (Test-EnvFlag "RELAYBOOK_UNINSTALL")
$DoNoGraphify = [bool]$NoGraphify -or (Test-EnvFlag "RELAYBOOK_NO_GRAPHIFY")
$DoWithRtk = [bool]$WithRtk -or (Test-EnvFlag "RELAYBOOK_WITH_RTK")
$DoYes = [bool]$Yes -or (Test-EnvFlag "RELAYBOOK_YES")

function Get-RelaybookArch {
    $raw = [Environment]::GetEnvironmentVariable("PROCESSOR_ARCHITEW6432")
    if ([string]::IsNullOrEmpty($raw)) {
        $raw = [Environment]::GetEnvironmentVariable("PROCESSOR_ARCHITECTURE")
    }
    switch -Regex ("$raw") {
        "^(AMD64|x86_64)$" { return "x86_64" }
        "^(ARM64|aarch64)$" { return "aarch64" }
        default {
            throw "unsupported architecture: $raw (supported: x86_64, aarch64)"
        }
    }
}

function Get-InstallDir {
    $override = [Environment]::GetEnvironmentVariable("RELAYBOOK_INSTALL_DIR")
    if (-not [string]::IsNullOrEmpty($override)) {
        return $override.TrimEnd('\')
    }
    return (Join-Path $env:LOCALAPPDATA "Programs\relaybook\bin")
}

function Get-ManifestPath {
    return (Join-Path $env:LOCALAPPDATA "relaybook\install.json")
}

function Read-InstallManifest {
    $path = Get-ManifestPath
    if (-not (Test-Path -LiteralPath $path)) {
        return $null
    }
    return (Get-Content -LiteralPath $path -Raw | ConvertFrom-Json)
}

function Write-InstallManifest {
    param(
        [string]$Binary,
        [string]$Graphify,
        [string]$Rtk,
        [bool]$RtkInit,
        [bool]$PathAdded,
        [string]$PathDir
    )
    $path = Get-ManifestPath
    $parent = Split-Path -Parent $path
    if (-not (Test-Path -LiteralPath $parent)) {
        New-Item -ItemType Directory -Path $parent | Out-Null
    }
    $payload = [ordered]@{
        binary     = $Binary
        graphify   = $Graphify
        rtk        = $Rtk
        rtk_init   = $RtkInit
        path_added = $PathAdded
        path_dir   = $PathDir
    }
    $json = ($payload | ConvertTo-Json) + "`n"
    $utf8 = New-Object System.Text.UTF8Encoding $false
    [System.IO.File]::WriteAllText($path, $json, $utf8)
}

function Get-UserPathEntry {
    $user = [Environment]::GetEnvironmentVariable("Path", "User")
    if ([string]::IsNullOrEmpty($user)) {
        return @()
    }
    return @($user.Split(";") | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
}

function ConvertTo-PathKey {
    param([string]$Path)
    return $Path.Trim().Replace("/", "\").TrimEnd("\")
}

function Test-SamePath {
    param([string]$Left, [string]$Right)
    return (ConvertTo-PathKey $Left).Equals((ConvertTo-PathKey $Right), [System.StringComparison]::OrdinalIgnoreCase)
}

function Add-UserPath {
    param([string]$Dir)
    $items = @(Get-UserPathEntry)
    foreach ($item in $items) {
        if (Test-SamePath $item $Dir) {
            return $false
        }
    }
    $normalized = ConvertTo-PathKey $Dir
    $updated = $normalized
    if ($items.Count -gt 0) {
        $updated = $normalized + ";" + ($items -join ";")
    }
    [Environment]::SetEnvironmentVariable("Path", $updated, "User")
    return $true
}

function Remove-UserPath {
    param([string]$Dir)
    $items = @(Get-UserPathEntry)
    $kept = @()
    $found = $false
    foreach ($item in $items) {
        if (Test-SamePath $item $Dir) {
            $found = $true
        }
        else {
            $kept += $item
        }
    }
    if (-not $found) {
        return $false
    }
    [Environment]::SetEnvironmentVariable("Path", ($kept -join ";"), "User")
    return $true
}

function Confirm-Tool {
    param([string]$Prompt, [string]$CommandText)
    if ($DoYes) {
        return $true
    }
    if ([Console]::IsInputRedirected) {
        Write-Output $CommandText
        return $false
    }
    $answer = Read-Host "$Prompt [y/N]"
    return $answer -match "^(y|yes)$"
}

function Get-CommandPath {
    param([string]$Name)
    $command = Get-Command -Name $Name -ErrorAction SilentlyContinue
    if ($null -eq $command) {
        return $null
    }
    return $command.Source
}

function Install-RelaybookBinary {
    param([string]$Destination)
    $arch = Get-RelaybookArch
    $name = "relaybook-windows-$arch.zip"
    $base = [Environment]::GetEnvironmentVariable("RELAYBOOK_RELEASE_BASE")
    if ([string]::IsNullOrEmpty($base)) {
        $base = "https://github.com/Spottie97/relaybook/releases/latest/download"
    }
    $temp = Join-Path ([System.IO.Path]::GetTempPath()) ("relaybook-install-" + [guid]::NewGuid().ToString("n"))
    New-Item -ItemType Directory -Path $temp | Out-Null
    try {
        $zip = Join-Path $temp $name
        if (Test-Path -LiteralPath $base -PathType Container) {
            $source = Join-Path $base $name
            if (-not (Test-Path -LiteralPath $source)) {
                throw "could not download $source"
            }
            Copy-Item -LiteralPath $source -Destination $zip
        }
        else {
            $url = ($base.TrimEnd("/")) + "/" + $name
            try {
                Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
            }
            catch {
                throw "could not download $url"
            }
        }
        Expand-Archive -LiteralPath $zip -DestinationPath (Join-Path $temp "out") -Force
        $extracted = Join-Path $temp "out\relaybook.exe"
        if (-not (Test-Path -LiteralPath $extracted)) {
            throw "archive did not contain relaybook.exe"
        }
        $parent = Split-Path -Parent $Destination
        if (-not (Test-Path -LiteralPath $parent)) {
            New-Item -ItemType Directory -Path $parent | Out-Null
        }
        $staged = $Destination + ".new"
        Copy-Item -LiteralPath $extracted -Destination $staged -Force
        Move-Item -LiteralPath $staged -Destination $Destination -Force
    }
    finally {
        if (Test-Path -LiteralPath $temp) {
            Remove-Item -LiteralPath $temp -Recurse -Force
        }
    }
}

function Uninstall-RelaybookMachine {
    $manifest = Read-InstallManifest
    $installDir = Get-InstallDir
    $defaultBinary = Join-Path $installDir "relaybook.exe"
    if ($null -eq $manifest -and -not (Test-Path -LiteralPath $defaultBinary)) {
        Write-Output "nothing installed"
        return
    }
    $binary = $defaultBinary
    $graphify = "none"
    $rtk = "none"
    $rtkInit = $false
    $pathAdded = $false
    $pathDir = $installDir
    if ($null -ne $manifest) {
        if (-not [string]::IsNullOrEmpty($manifest.binary)) {
            $binary = [string]$manifest.binary
        }
        $graphify = [string]$manifest.graphify
        $rtk = [string]$manifest.rtk
        $rtkInit = [bool]$manifest.rtk_init
        $pathAdded = [bool]$manifest.path_added
        if (-not [string]::IsNullOrEmpty($manifest.path_dir)) {
            $pathDir = [string]$manifest.path_dir
        }
    }
    if (Test-Path -LiteralPath $binary) {
        Remove-Item -LiteralPath $binary -Force
        Write-Output "removed $binary"
    }
    $found = Get-CommandPath "relaybook"
    if ($null -ne $found) {
        if ($found -match "[\\/]\.cargo[\\/]bin[\\/]relaybook(\.exe)?$") {
            Write-Output "cargo uninstall relaybook"
        }
        else {
            Write-Output "kept $found (not installed by install.ps1)"
        }
    }
    switch ($graphify) {
        "pre-existing" { Write-Output "kept graphify (already installed)" }
        "uv" {
            if (Confirm-Tool "Remove Graphify installed by the installer?" "uv tool uninstall graphifyy") {
                & uv tool uninstall graphifyy
                Write-Output "removed graphifyy"
            }
        }
        "pipx" {
            if (Confirm-Tool "Remove Graphify installed by the installer?" "pipx uninstall graphifyy") {
                & pipx uninstall graphifyy
                Write-Output "removed graphifyy"
            }
        }
    }
    if ($rtkInit) {
        if (Confirm-Tool "Remove the RTK Claude hook?" "rtk init -g --uninstall") {
            & rtk init -g --uninstall
            Write-Output "removed rtk hook"
        }
    }
    switch ($rtk) {
        "pre-existing" { Write-Output "kept rtk (already installed)" }
        "winget" {
            if (Confirm-Tool "Remove RTK installed by the installer?" "winget uninstall --id rtk-ai.rtk -e") {
                & winget uninstall --id rtk-ai.rtk -e
                Write-Output "removed rtk"
            }
        }
        "brew" {
            if (Confirm-Tool "Remove RTK installed by the installer?" "brew uninstall rtk") {
                & brew uninstall rtk
                Write-Output "removed rtk"
            }
        }
    }
    if ($pathAdded) {
        if (Remove-UserPath $pathDir) {
            Write-Output "removed PATH entry $pathDir"
        }
    }
    Write-Output "project files stay until you run relaybook uninstall in each project"
    $manifestPath = Get-ManifestPath
    if (Test-Path -LiteralPath $manifestPath) {
        Remove-Item -LiteralPath $manifestPath -Force
    }
    $manifestDir = Split-Path -Parent $manifestPath
    if ((Test-Path -LiteralPath $manifestDir) -and -not (Get-ChildItem -LiteralPath $manifestDir -Force)) {
        Remove-Item -LiteralPath $manifestDir -Force
    }
}

if ($env:OS -ne "Windows_NT") {
    throw "install.ps1 is for Windows. On macOS and Linux, run install.sh."
}

if ($DoUninstall) {
    Uninstall-RelaybookMachine
    return
}

$installDir = Get-InstallDir
$binary = Join-Path $installDir "relaybook.exe"
$existed = Test-Path -LiteralPath $binary
if ($existed -and -not $DoUpgrade) {
    Write-Output "already installed; re-run with -Upgrade to update"
}
elseif ($existed) {
    $oldVersion = & $binary --version
    Install-RelaybookBinary -Destination $binary
    $newVersion = & $binary --version
    Write-Output "$oldVersion -> $newVersion"
}
else {
    Install-RelaybookBinary -Destination $binary
    Write-Output "installed relaybook to $binary"
}

$previous = Read-InstallManifest
$pathAdded = $false
$pathDir = $installDir
if ($null -ne $previous -and [bool]$previous.path_added) {
    $pathAdded = $true
    if (-not [string]::IsNullOrEmpty($previous.path_dir)) {
        $pathDir = [string]$previous.path_dir
    }
}
if (Add-UserPath $installDir) {
    $pathAdded = $true
    $pathDir = $installDir
    Write-Output "Open a new terminal to use relaybook."
}
elseif (-not $existed) {
    Write-Output "Open a new terminal to use relaybook."
}

$graphify = "none"
$rtk = "none"
$rtkInit = $false
if ($null -ne (Get-CommandPath "graphify")) {
    $graphify = "pre-existing"
}
if ($null -ne (Get-CommandPath "rtk")) {
    $rtk = "pre-existing"
}

if ($DoNoGraphify) {
    Write-Output "skipped graphify (-NoGraphify)"
}
elseif ($DoUpgrade -and $null -ne (Get-CommandPath "graphify")) {
    if ($null -ne (Get-CommandPath "uv")) {
        & uv tool upgrade graphifyy
        if ($LASTEXITCODE -ne 0) {
            Write-Output "warning: graphify upgrade failed; Relaybook is still installed"
        }
    }
    elseif ($null -ne (Get-CommandPath "pipx")) {
        & pipx upgrade graphifyy
        if ($LASTEXITCODE -ne 0) {
            Write-Output "warning: graphify upgrade failed; Relaybook is still installed"
        }
    }
    else {
        Write-Output "warning: could not tell how graphify was installed; skipped the graphify upgrade"
    }
}
elseif ($null -ne (Get-CommandPath "graphify")) {
    Write-Output "graphify already installed"
}
else {
    if ($null -ne (Get-CommandPath "uv")) {
        & uv tool install graphifyy
        if ($LASTEXITCODE -eq 0) {
            $graphify = "uv"
        }
        else {
            Write-Output "graphify install failed; Relaybook is still installed"
        }
    }
    elseif ($null -ne (Get-CommandPath "pipx")) {
        & pipx install graphifyy
        if ($LASTEXITCODE -eq 0) {
            $graphify = "pipx"
        }
        else {
            Write-Output "graphify install failed; Relaybook is still installed"
        }
    }
    else {
        Write-Output "graphify was not installed. Install uv or pipx, then run: uv tool install graphifyy"
        Write-Output "or: pipx install graphifyy"
    }
}

if ($DoWithRtk) {
    if ($null -ne (Get-CommandPath "rtk")) {
        Write-Output "rtk already installed"
    }
    elseif ($null -ne (Get-CommandPath "winget")) {
        & winget install --id rtk-ai.rtk -e --accept-source-agreements --accept-package-agreements
        if ($LASTEXITCODE -eq 0) {
            $rtk = "winget"
        }
        else {
            Write-Output "warning: rtk install failed; Relaybook is still installed"
        }
    }
    else {
        Write-Output "rtk was not installed. See https://www.rtk-ai.app/ or run: winget install rtk-ai.rtk"
    }
    if ($null -ne (Get-CommandPath "rtk")) {
        if ($DoYes) {
            & rtk init -g
            if ($LASTEXITCODE -eq 0) {
                $rtkInit = $true
            }
            else {
                Write-Output "warning: rtk init failed; Relaybook is still installed"
            }
        }
        elseif (Confirm-Tool "Add the RTK hook to your global Claude settings?" "rtk init -g") {
            & rtk init -g
            if ($LASTEXITCODE -eq 0) {
                $rtkInit = $true
            }
            else {
                Write-Output "warning: rtk init failed; Relaybook is still installed"
            }
        }
        else {
            Write-Output "skipped rtk init; run: rtk init -g"
        }
    }
}

Write-InstallManifest -Binary $binary -Graphify $graphify -Rtk $rtk -RtkInit $rtkInit -PathAdded $pathAdded -PathDir $pathDir
& $binary --version
if (-not $DoNoGraphify) {
    $graphifyPath = Get-CommandPath "graphify"
    if ($null -ne $graphifyPath) {
        & $graphifyPath --version
        if ($LASTEXITCODE -ne 0) {
            Write-Output "graphify installed"
        }
    }
}
