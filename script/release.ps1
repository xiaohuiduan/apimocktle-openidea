# Publishes a release: builds, signs, creates the GitHub release and attaches
# the package.
#
#   pwsh script/release.ps1                 # release the version in plugin.toml
#   pwsh script/release.ps1 -DryRun         # build and sign only, print the rest
#
# This automates the registry manual's steps 3 and 4:
#
#   openidea-plugin build .
#   openidea-plugin pack . --key alice.key --publisher alice
#   # attach the .oiplugin to a release of your repository
#
# The signing key stays on this machine: it is not in the repository (`.key` is
# ignored) and not in CI secrets, because anyone who can read it can publish
# packages signed as this publisher. So the release runs here, as one command,
# instead of from a workflow.
#
# The registry pull request is deliberately not automated: the manual asks for a
# maintainer to review the source and the capabilities, and a bot opening the
# pull request would not change that.
param(
    [string]$Key = "$env:USERPROFILE\.openidea-keys\apimocktle.key",
    [string]$Publisher = 'apimocktle',
    [switch]$DryRun
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root

if (-not (Test-Path $Key)) { throw "signing key not found: $Key" }

$version = (Select-String -Path plugin.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$tag = "v$version"
$pkg = Join-Path $root "apimocktle.openidea-$version.oiplugin"

# A release must point at a commit, and the tag must already exist: publishing
# from an uncommitted tree would ship bytes that do not match any commit.
$dirty = git status --porcelain
if ($dirty) { throw "the working tree is dirty; commit first:`n$dirty" }
$head = git rev-parse HEAD
# `git rev-parse <tag>` on an *annotated* tag returns the tag object's own SHA,
# not the commit it points at, so comparing it with HEAD fails for annotated tags
# while passing for lightweight ones. `<tag>^{commit}` peels either kind down to
# the commit, which is what the check is actually about.
$tagCommit = git rev-parse "$tag^{commit}" 2>$null
if (-not $tagCommit) { throw "tag $tag does not exist; create it on the commit to release" }
if ($tagCommit -ne $head) { throw "tag $tag points at $tagCommit, but HEAD is $head" }

Write-Host "ApiMocktle $version from $($head.Substring(0, 7))"

# Build through the documented command so the component is exactly what any
# contributor gets from this source.
& openidea-plugin build .
if ($LASTEXITCODE -ne 0) { throw 'openidea-plugin build failed' }

if (Test-Path $pkg) { Remove-Item $pkg -Force }
& openidea-plugin pack . --key $Key --publisher $Publisher
if ($LASTEXITCODE -ne 0) { throw 'openidea-plugin pack failed' }
if (-not (Test-Path $pkg)) { throw "pack did not produce $pkg" }

$sha = (Get-FileHash $pkg -Algorithm SHA256).Hash.ToLower()
$size = (Get-Item $pkg).Length
Write-Host "package : $pkg"
Write-Host "sha256  : $sha"
Write-Host "bytes   : $size"

if ($DryRun) {
    Write-Host ''
    Write-Host '-DryRun: built and signed; the release was not touched.'
    Write-Host "Next: push the tag and run without -DryRun, or release by hand."
    exit 0
}

# `gh` is not assumed: the API is called directly, with the token the git
# credential helper already stores.
$env:GIT_TERMINAL_PROMPT = '0'
$cred = ("protocol=https`nhost=github.com`n`n" | git credential fill) -split "`n"
$token = ($cred | Where-Object { $_ -match '^password=' }) -replace '^password=', ''
if (-not $token) { throw 'no GitHub token in the credential helper' }
$headers = @{ Authorization = "token $token"; 'User-Agent' = 'apimocktle-release'; Accept = 'application/vnd.github+json' }
$proxy = if ($env:HTTPS_PROXY) { $env:HTTPS_PROXY } else { $null }
$common = @{ Headers = $headers; TimeoutSec = 60 }
if ($proxy) { $common.Proxy = $proxy }

$repo = (git remote get-url origin) -replace '^https://github.com/', '' -replace '\.git$', ''
Write-Host "repo    : $repo"

# Read the notes straight from plugin.toml as UTF-8. Do NOT shell out to Python
# and capture stdout: PowerShell decodes a child process' output with the console
# code page, which on a Chinese Windows is GBK. The UTF-8 Chinese text then turns
# into replacement characters and, once posted, cannot be recovered — that is
# exactly how the v0.2.7 release body ended up full of U+FFFD.
$toml = [System.IO.File]::ReadAllText((Join-Path $root 'plugin.toml'), [System.Text.Encoding]::UTF8)
$notesMatch = [regex]::Match($toml, '(?m)^change-notes = "(.*)"\s*$')
if (-not $notesMatch.Success) { throw 'plugin.toml has no change-notes' }
$notes = $notesMatch.Groups[1].Value
# `(?m)` only: `(?s)` would let `.` cross newlines and swallow the rest of the file.
$expected = (Select-String -Path 'plugin.toml' -Pattern '^change-notes = "' | Measure-Object).Count
if ($expected -ne 1) { throw "plugin.toml must have exactly one change-notes (found $expected)" }
if ($notes.Length -lt 10 -or $notes.Length -gt 2000) { throw "change-notes looks wrong: $($notes.Length) characters" }
if ($notes -match "`u{FFFD}") { throw 'change-notes contains U+FFFD already; fix plugin.toml before releasing' }
Write-Host "notes   : $($notes.Length) chars, starts: $($notes.Substring(0, [Math]::Min(30, $notes.Length)))..."

# Send the JSON as UTF-8 bytes, never as a .NET string. `Invoke-RestMethod -Body
# <string>` encodes with ISO-8859-1, which turns every Chinese character into
# '?', and the damage is permanent once stored. Passing bytes removes the guess.
#
# `Write-Output -NoEnumerate` matters: `return $bytes` from a function unrolls the
# array into Object[], and -Body then serialises an object array instead of
# writing the bytes, so the PATCH silently stores nothing.
function ConvertTo-Utf8JsonBody($object) {
    $json = $object | ConvertTo-Json -Compress
    Write-Output -NoEnumerate ([System.Text.Encoding]::UTF8.GetBytes($json))
}

$release = $null
try {
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/tags/$tag" @common
    Write-Host "release $tag already exists; refreshing its notes and package asset"
    # The notes belong to the tag being released. If they were edited since the
    # release was created (or created wrong), refresh them instead of leaving
    # whatever was stored first.
    $patch = ConvertTo-Utf8JsonBody @{ body = $notes; name = $tag }
    $patched = @{} + $common
    $patched.Method = 'Patch'
    $patched.Body = $patch
    $patched.ContentType = 'application/json; charset=utf-8'
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/$($release.id)" @patched
    Write-Host "notes refreshed ($($release.body.Length) chars)"
    foreach ($asset in $release.assets) {
        if ($asset.name -eq (Split-Path $pkg -Leaf)) {
            Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/assets/$($asset.id)" -Method Delete @common
        }
    }
} catch {
    if ($_.Exception.Response.StatusCode.value__ -ne 404) { throw }
    $body = ConvertTo-Utf8JsonBody @{ tag_name = $tag; name = $tag; body = $notes; draft = $false; prerelease = $false }
    $create = @{} + $common
    $create.Method = 'Post'
    $create.Body = $body
    $create.ContentType = 'application/json; charset=utf-8'
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases" @create
    Write-Host "release $tag created ($($release.body.Length) chars)"
}

# Prove the stored notes survived the round trip before reporting success.
if ($release.body -ne $notes) {
    throw "the release notes did not survive the upload: stored $($release.body.Length) chars, expected $($notes.Length)"
}

$upload = @{ Headers = $headers; TimeoutSec = 300; Method = 'Post'; InFile = $pkg; ContentType = 'application/octet-stream' }
if ($proxy) { $upload.Proxy = $proxy }
$asset = Invoke-RestMethod -Uri "https://uploads.github.com/repos/$repo/releases/$($release.id)/assets?name=$(Split-Path $pkg -Leaf)" @upload
Write-Host "uploaded: $($asset.name) ($($asset.size) bytes)"

Write-Host ''
Write-Host "Release: $($release.html_url)"
Write-Host ''
Write-Host 'Next, per the registry manual: add the version to'
Write-Host "  plugins/apimocktle.openidea.toml"
Write-Host 'in l0ng-ai/openidea-plugins with'
Write-Host "  version = `"$version`""
Write-Host "  package = `"https://github.com/$repo/releases/download/$tag/$(Split-Path $pkg -Leaf)`""
Write-Host "  sha256  = `"$sha`""
Write-Host 'and open the pull request.'
