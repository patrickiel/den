# Build an extension and side-load it into den, which picks it up at its next start.
#   .\scripts\sideload.ps1 examples\hello-extension
# Without a folder it offers the repository's extensions, or takes a path.
# It goes in as `<id>.pending`, as an install from the Extensions view does, so den can stay
# open (the loaded DLL stays locked until den exits).
param([string]$Path)
$ErrorActionPreference = "Stop"

if (-not $Path) {
    $root = Split-Path $PSScriptRoot
    $found = @(Get-ChildItem (Join-Path $root "*\*\extension.json") | ForEach-Object { $_.DirectoryName })
    for ($i = 0; $i -lt $found.Count; $i++) { Write-Host "  $($i + 1)) $($found[$i].Substring($root.Length + 1))" }
    $answer = (Read-Host "Extension (number, or a folder path)").Trim().Trim('"')
    $Path = if ($answer -match '^\d+$' -and [int]$answer -ge 1 -and [int]$answer -le $found.Count) { $found[[int]$answer - 1] } else { $answer }
}

$manifestPath = Join-Path $Path "extension.json"
if (-not (Test-Path $manifestPath)) { throw "No extension.json in $Path" }
$cargoToml = Join-Path $Path "Cargo.toml"
$id = (Get-Content $manifestPath -Raw | ConvertFrom-Json).id

cargo build --release --manifest-path $cargoToml
if ($LASTEXITCODE) { exit $LASTEXITCODE }
$target = (cargo metadata --format-version 1 --no-deps --manifest-path $cargoToml | ConvertFrom-Json).target_directory
$dll = Join-Path $target "release\$($id -replace '-', '_').dll"

$pending = Join-Path $env:APPDATA "den\extensions\$id.pending"
Remove-Item $pending -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $pending | Out-Null
Copy-Item $manifestPath, $dll $pending
$readme = Join-Path $Path "README.md"
if (Test-Path $readme) { Copy-Item $readme $pending }
$icon = (Get-Content $manifestPath -Raw | ConvertFrom-Json).icon
if ($icon) {
    $dest = Join-Path $pending $icon
    New-Item -ItemType Directory -Force (Split-Path $dest) | Out-Null
    Copy-Item (Join-Path $Path $icon) $dest
}

Write-Host "Staged $id. Restart den to load it; see $env:APPDATA\den\extensions.log"
