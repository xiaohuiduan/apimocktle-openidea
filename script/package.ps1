# Builds the component and packs the installable archive.
#
# Build and pack are the two commands the registry manual documents; this script
# only wraps them so the local zip has the layout "Install Plugin from Package
# File" reads. Publishing is the manual's flow, nothing extra:
#
#   openidea-plugin build .
#   openidea-plugin pack . --key <key> --publisher apimocktle
#
# `openidea-plugin build` writes dist/plugin.wasm itself, so nothing here
# post-processes the component: the bytes a user installs are exactly what that
# command produces from this source.
param([string]$Toolchain = '')
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root

$version = (Select-String -Path plugin.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
Write-Host "ApiMocktle $version"

# A machine without the MSVC linker (PATH `link.exe` is coreutils') uses the GNU
# toolchain instead, e.g. `-Toolchain 1.96.0-x86_64-pc-windows-gnu`.
$cargo = @()
if ($Toolchain) { $cargo += "+$Toolchain" }
& cargo @cargo build --release --target wasm32-wasip2
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { 'target' }
New-Item -ItemType Directory -Force -Path dist | Out-Null
Copy-Item "$targetDir/wasm32-wasip2/release/openidea_apimocktle.wasm" dist/plugin.wasm -Force

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
