param(
    [Parameter(Mandatory = $true)]
    [ValidateSet("Install", "Finish")]
    [string]$Phase
)

$ErrorActionPreference = "Stop"
$root = (Join-Path $env:RUNNER_TEMP "relaybook-installer").Replace("\", "/")
$local = (Join-Path $root "local").Replace("\", "/")
$bin = (Join-Path $root "bin").Replace("\", "/")
$dist = (Join-Path $root "dist").Replace("\", "/")
$savedPath = (Join-Path $root "user-path.txt").Replace("\", "/")

function Set-InstallerEnv {
    $env:LOCALAPPDATA = $local
    $env:RELAYBOOK_INSTALL_DIR = $bin
    $env:RELAYBOOK_RELEASE_BASE = $dist
    Remove-Item Env:RELAYBOOK_UPGRADE -ErrorAction SilentlyContinue
    Remove-Item Env:RELAYBOOK_UNINSTALL -ErrorAction SilentlyContinue
    Remove-Item Env:RELAYBOOK_NO_GRAPHIFY -ErrorAction SilentlyContinue
    Remove-Item Env:RELAYBOOK_WITH_RTK -ErrorAction SilentlyContinue
    Remove-Item Env:RELAYBOOK_YES -ErrorAction SilentlyContinue
}

function Invoke-Installer {
    param([string[]]$Arguments, [string]$File)
    & $File @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$File $($Arguments -join ' ') exited $LASTEXITCODE"
    }
}

function New-ReleaseZip {
    $stage = Join-Path $root "stage"
    New-Item -ItemType Directory -Force -Path $dist, $local, $bin, $stage | Out-Null
    Copy-Item -LiteralPath "target\debug\relaybook.exe" -Destination (Join-Path $stage "relaybook.exe") -Force
    $arch = "x86_64"
    if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") {
        $arch = "aarch64"
    }
    $zip = Join-Path $dist "relaybook-windows-$arch.zip"
    if (Test-Path -LiteralPath $zip) {
        Remove-Item -LiteralPath $zip -Force
    }
    Push-Location $stage
    try {
        & tar.exe -a -c -f $zip relaybook.exe
        if ($LASTEXITCODE -ne 0) {
            throw "tar exited $LASTEXITCODE"
        }
    }
    finally {
        Pop-Location
    }
}

function Test-UserPathContains {
    param([string]$Dir, [bool]$Expected)
    $user = [Environment]::GetEnvironmentVariable("Path", "User")
    $norm = $Dir.Replace("/", "\").TrimEnd("\")
    $found = $false
    foreach ($item in @($user -split ";")) {
        if ($item -and ($item.Replace("/", "\").TrimEnd("\") -ieq $norm)) {
            $found = $true
        }
    }
    if ($found -ne $Expected) {
        throw "user PATH contains $Dir is $found, expected $Expected. PATH=$user"
    }
}

if ($Phase -eq "Install") {
    $existing = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($null -eq $existing) {
        $existing = ""
    }
    New-Item -ItemType Directory -Force -Path $root | Out-Null
    Set-Content -LiteralPath $savedPath -Value $existing -Encoding utf8
    New-ReleaseZip
    Set-InstallerEnv
    Invoke-Installer -File "pwsh" -Arguments @("-NoProfile", "-File", "install.ps1", "-NoGraphify", "-Yes")
    if (-not (Test-Path -LiteralPath (Join-Path $bin "relaybook.exe"))) {
        throw "install did not write relaybook.exe"
    }
    $manifest = Get-Content -LiteralPath (Join-Path $local "relaybook\install.json") -Raw
    if ($manifest -notmatch '"graphify":\s*"none"') {
        throw "manifest did not record graphify none: $manifest"
    }
    if ($manifest -notmatch '"path_added":\s*true') {
        throw "manifest did not record the PATH edit: $manifest"
    }
    Test-UserPathContains -Dir $bin -Expected $true
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $machinePath = [Environment]::GetEnvironmentVariable("Path", "Machine")
    $env:PATH = "$userPath;$machinePath"
    & (Join-Path $bin "relaybook.exe") --version
    if ($LASTEXITCODE -ne 0) {
        throw "relaybook --version exited $LASTEXITCODE"
    }
    Add-Content -Path $env:GITHUB_PATH -Value $bin
    return
}

Set-InstallerEnv
$againLines = & pwsh -NoProfile -File install.ps1 -NoGraphify -Yes 2>&1
$againCode = $LASTEXITCODE
$again = $againLines | Out-String
if ($againCode -ne 0) {
    throw "re-run exited $againCode. $again"
}
if ($again -notmatch "already installed") {
    throw "re-run did not report already installed: $again"
}
$upgradedLines = & pwsh -NoProfile -File install.ps1 -NoGraphify -Upgrade -Yes 2>&1
$upgradedCode = $LASTEXITCODE
$upgraded = $upgradedLines | Out-String
if ($upgradedCode -ne 0) {
    throw "upgrade exited $upgradedCode. $upgraded"
}
if ($upgraded -notmatch "->") {
    throw "upgrade did not print the version change: $upgraded"
}

try {
    $gitBash = "C:\Program Files\Git\bin\bash.exe"
    if (-not (Test-Path -LiteralPath $gitBash)) {
        throw "Git Bash is not installed"
    }
    Invoke-Installer -File $gitBash -Arguments @("./install.sh", "--uninstall", "--yes")
    if (Test-Path -LiteralPath (Join-Path $bin "relaybook.exe")) {
        throw "install.sh --uninstall left relaybook.exe"
    }
    Test-UserPathContains -Dir $bin -Expected $false
    if (Test-Path -LiteralPath (Join-Path $local "relaybook\install.json")) {
        throw "install.sh --uninstall left the manifest"
    }

    Invoke-Installer -File $gitBash -Arguments @("./install.sh", "--no-graphify", "--yes")
    if (-not (Test-Path -LiteralPath (Join-Path $bin "relaybook.exe"))) {
        throw "install.sh did not write relaybook.exe"
    }
    Test-UserPathContains -Dir $bin -Expected $true
    Invoke-Installer -File "pwsh" -Arguments @("-NoProfile", "-File", "install.ps1", "-Uninstall", "-Yes")
    if (Test-Path -LiteralPath (Join-Path $bin "relaybook.exe")) {
        throw "install.ps1 -Uninstall left a Git Bash install"
    }
    Test-UserPathContains -Dir $bin -Expected $false

    $fake = Join-Path $root "fake"
    New-Item -ItemType Directory -Force -Path $fake | Out-Null
    Set-Content -LiteralPath (Join-Path $fake "graphify.cmd") -Value "@echo off`r`nexit /b 0" -Encoding ascii
    $env:PATH = "$fake;" + $env:PATH
    $env:LOCALAPPDATA = Join-Path $root "local-graphify"
    $keptLines = & pwsh -NoProfile -File install.ps1 -Yes 2>&1
    $keptCode = $LASTEXITCODE
    $kept = $keptLines | Out-String
    if ($keptCode -ne 0) {
        throw "graphify install exited $keptCode. $kept"
    }
    $graphManifest = Get-Content -LiteralPath (Join-Path $env:LOCALAPPDATA "relaybook\install.json") -Raw
    if ($graphManifest -notmatch '"graphify":\s*"pre-existing"') {
        throw "pre-existing graphify was not recorded: $graphManifest"
    }
    $removedLines = & pwsh -NoProfile -File install.ps1 -Uninstall -Yes 2>&1
    $removedCode = $LASTEXITCODE
    $removed = $removedLines | Out-String
    if ($removedCode -ne 0) {
        throw "graphify uninstall exited $removedCode. $removed"
    }
    if ($removed -notmatch "kept graphify") {
        throw "uninstall removed a pre-existing graphify: $removed"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $fake "graphify.cmd"))) {
        throw "graphify.cmd was deleted"
    }
}
finally {
    $original = Get-Content -LiteralPath $savedPath -Raw
    if ($null -eq $original) {
        $original = ""
    }
    [Environment]::SetEnvironmentVariable("Path", $original.Trim(), "User")
}
