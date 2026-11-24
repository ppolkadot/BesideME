# Builds the release binaries and the Manga Watcher setup wizard.
# Output: dist\MangaWatcher-Setup-<version>.exe
# Requires Inno Setup 6.7+ (winget install JRSoftware.InnoSetup).
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    cargo build --release --bins
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

    $version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' |
        Select-Object -First 1).Matches[0].Groups[1].Value

    $iscc = @(
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
    ) | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $iscc) { throw "Inno Setup 6 not found. Install it with: winget install JRSoftware.InnoSetup" }

    & $iscc "/DAppVersion=$version" "installer\MangaWatcher.iss"
    if ($LASTEXITCODE -ne 0) { throw "Inno Setup compile failed" }
    Write-Host "Installer: $root\dist\MangaWatcher-Setup-$version.exe"
}
finally {
    Pop-Location
}
