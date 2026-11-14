# Builds the plugin component and packs the installable archive.
#   pwsh script/package.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root

$version = (Select-String -Path plugin.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
Write-Host "ApiMocktle $version"

cargo build --release --target wasm32-wasip2
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

New-Item -ItemType Directory -Force -Path dist | Out-Null
Copy-Item target/wasm32-wasip2/release/openidea_apimocktle.wasm dist/plugin.wasm -Force

$stamp = Get-Date -Format 'HH:mm:ss'
Set-Content -Path dist/plugin.wasm.sources -Value "built $stamp from $(git rev-parse --short HEAD 2>$null)" -NoNewline

$out = Join-Path 'dist' "apimocktle-openidea-$version.zip"
if (Test-Path $out) { Remove-Item $out }

# The archive keeps the package layout: plugin.toml at the root and the
# component at the `wasm` path the manifest names.
$stage = Join-Path $env:TEMP "apimocktle-pack-$version"
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path (Join-Path $stage 'dist') | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $stage 'icons') | Out-Null
Copy-Item 'plugin.toml', 'settings.schema.json', 'README.md' $stage
Copy-Item 'dist/plugin.wasm' (Join-Path $stage 'dist/plugin.wasm')
Copy-Item 'icons/*.svg' (Join-Path $stage 'icons')
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $out
Remove-Item $stage -Recurse -Force

Write-Host "packed $out ($([math]::Round((Get-Item $out).Length / 1KB, 1)) KiB)"
