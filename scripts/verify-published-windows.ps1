# Install the published GitHub release on a fresh Windows profile.
# PowerShell uses the irm | iex one-liner. Git Bash uses the curl | sh one-liner.
$ErrorActionPreference = "Stop"

$root = Join-Path $env:RUNNER_TEMP "relaybook-published-verify"
$local = Join-Path $root "local"
$repo = Join-Path $root "repo"
$savedPath = Join-Path $root "user-path.txt"
$installDir = Join-Path $local "Programs\relaybook\bin"
$binary = Join-Path $installDir "relaybook.exe"

function Set-FreshProfile {
    New-Item -ItemType Directory -Force -Path $local | Out-Null
    $env:LOCALAPPDATA = $local
    Remove-Item Env:RELAYBOOK_RELEASE_BASE -ErrorAction SilentlyContinue
    Remove-Item Env:RELAYBOOK_INSTALL_DIR -ErrorAction SilentlyContinue
    Remove-Item Env:RELAYBOOK_UPGRADE -ErrorAction SilentlyContinue
    Remove-Item Env:RELAYBOOK_UNINSTALL -ErrorAction SilentlyContinue
    Remove-Item Env:RELAYBOOK_NO_GRAPHIFY -ErrorAction SilentlyContinue
    Remove-Item Env:RELAYBOOK_WITH_RTK -ErrorAction SilentlyContinue
    Remove-Item Env:RELAYBOOK_YES -ErrorAction SilentlyContinue
}

function Invoke-NewShell {
    param([string]$Command)
    $user = [Environment]::GetEnvironmentVariable("Path", "User")
    $machine = [Environment]::GetEnvironmentVariable("Path", "Machine")
    $previous = $env:PATH
    try {
        $env:PATH = "$user;$machine"
        $output = & powershell.exe -NoProfile -Command $Command 2>&1 | Out-String
        if ($LASTEXITCODE -ne 0) {
            throw "new shell failed ($LASTEXITCODE): $output"
        }
        return $output
    }
    finally {
        $env:PATH = $previous
    }
}

function Assert-UserPath {
    param([bool]$Expected)
    $user = [Environment]::GetEnvironmentVariable("Path", "User")
    $norm = $installDir.TrimEnd("\")
    $found = $false
    foreach ($item in @($user -split ";")) {
        if ($item -and ($item.TrimEnd("\") -ieq $norm)) {
            $found = $true
        }
    }
    if ($found -ne $Expected) {
        throw "user PATH contains $installDir is $found, expected $Expected. PATH=$user"
    }
}

New-Item -ItemType Directory -Force -Path $root | Out-Null
$original = [Environment]::GetEnvironmentVariable("Path", "User")
if ($null -eq $original) {
    $original = ""
}
Set-Content -LiteralPath $savedPath -Value $original -Encoding utf8

try {
    Set-FreshProfile
    irm https://raw.githubusercontent.com/Spottie97/relaybook/main/install.ps1 | iex
    if (-not (Test-Path -LiteralPath $binary)) {
        throw "PowerShell install did not write $binary"
    }
    Assert-UserPath -Expected $true
    $version = Invoke-NewShell -Command "relaybook --version"
    if ($version -notmatch "relaybook 0\.3\.0") {
        throw "new shell version was not 0.3.0: $version"
    }

    New-Item -ItemType Directory -Force -Path $repo | Out-Null
    Push-Location $repo
    try {
        & git init -q
        if ($LASTEXITCODE -ne 0) { throw "git init exited $LASTEXITCODE" }
        & $binary setup --no-graphify
        if ($LASTEXITCODE -ne 0) { throw "setup exited $LASTEXITCODE" }
        if (-not (Test-Path -LiteralPath (Join-Path $repo ".relaybook\ledger.db"))) {
            throw "setup did not create the ledger"
        }
        & $binary uninstall --yes
        if ($LASTEXITCODE -ne 0) { throw "uninstall exited $LASTEXITCODE" }
    }
    finally {
        Pop-Location
    }

    & ([scriptblock]::Create((irm https://raw.githubusercontent.com/Spottie97/relaybook/main/install.ps1))) -Uninstall -Yes
    if (Test-Path -LiteralPath $binary) {
        throw "PowerShell uninstall left $binary"
    }
    Assert-UserPath -Expected $false

    $gitBash = "C:\Program Files\Git\bin\bash.exe"
    if (-not (Test-Path -LiteralPath $gitBash)) {
        throw "Git Bash is not installed"
    }
    $env:LOCALAPPDATA = $local
    & $gitBash -c "curl -fsSL https://raw.githubusercontent.com/Spottie97/relaybook/main/install.sh | sh"
    if ($LASTEXITCODE -ne 0) {
        throw "Git Bash install.sh exited $LASTEXITCODE"
    }
    if (-not (Test-Path -LiteralPath $binary)) {
        throw "Git Bash install did not write $binary"
    }
    $again = Invoke-NewShell -Command "relaybook --version"
    if ($again -notmatch "relaybook 0\.3\.0") {
        throw "PowerShell did not run the Git Bash install: $again"
    }
}
finally {
    $restore = Get-Content -LiteralPath $savedPath -Raw
    if ($null -eq $restore) {
        $restore = ""
    }
    [Environment]::SetEnvironmentVariable("Path", $restore.Trim(), "User")
}
