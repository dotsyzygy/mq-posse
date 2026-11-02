# Builds MQ2Posse.dll against the local MacroQuest (MQ_ROOT) and publishes it
# as a GitHub release tagged v<Cargo.toml version>. Run it through release.sh.
#
# -DryRun runs every check and the build, but doesn't tag, push or release.

param([switch]$DryRun)

$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)

function Fail([string]$message) {
    Write-Host "release: $message" -ForegroundColor Red
    exit 1
}

function Run([string]$exe, [string[]]$arguments) {
    & $exe @arguments
    if ($LASTEXITCODE -ne 0) { Fail "$exe $($arguments -join ' ') failed ($LASTEXITCODE)" }
}

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches.Groups[1].Value
if (-not $version) { Fail 'no version in Cargo.toml' }
$tag = "v$version"

# Release only what's on origin/main, so the tag names a commit anyone can build.
Run git @('fetch', '--quiet', '--tags', 'origin')
if ((git branch --show-current) -ne 'main') { Fail 'not on main' }
if (git status --porcelain) { Fail 'working tree has uncommitted changes' }
if ((git rev-parse HEAD) -ne (git rev-parse origin/main)) { Fail 'main differs from origin/main; push or pull first' }
if (git tag --list $tag) { Fail "$tag already exists; bump the version in Cargo.toml" }

$mqRoot = $env:MQ_ROOT
if (-not $mqRoot) {
    $config = Join-Path $HOME '.cargo/config.toml'
    if (Test-Path $config) {
        $mqRoot = (Select-String -Path $config -Pattern '^MQ_ROOT\s*=\s*"(.+)"' | Select-Object -First 1).Matches.Groups[1].Value
    }
}
if (-not $mqRoot) { Fail 'MQ_ROOT is not set, in the environment or ~/.cargo/config.toml' }
$mqCommit = git -C $mqRoot rev-parse --short HEAD
if ($LASTEXITCODE -ne 0) { Fail "can't read MacroQuest's commit in $mqRoot" }
$mqBranch = git -C $mqRoot branch --show-current
if (git -C $mqRoot status --porcelain --untracked-files=no) { Fail "MacroQuest at $mqRoot has local changes; release against a clean checkout" }
# MQMAIN_VERSION_BUILD is MacroQuest's CI run number (0 locally), so leave it out.
$versionInfo = Get-Content (Join-Path $mqRoot 'src/main/MQVersionInfo.h') -Raw
$mqVersion = (('MAJOR', 'MINOR', 'REV') | ForEach-Object {
    if ($versionInfo -notmatch "#define MQMAIN_VERSION_$_\s+(\d+)") { Fail "no MQMAIN_VERSION_$_ in MQVersionInfo.h" }
    $Matches[1]
}) -join '.'

Run cargo @('test', '--locked', '-p', 'posse-core', '-p', 'posse-shm')
Run cargo @('build', '--locked', '--release')

$out = 'target/i686-pc-windows-msvc/release'
$assets = @("$out/MQ2Posse.dll", "$out/MQ2Posse.pdb")
foreach ($asset in $assets) {
    if (-not (Test-Path $asset)) { Fail "$asset wasn't built" }
}

$notes = "**Requires MacroQuest $mqVersion, ``$mqBranch`` branch (macroquest/macroquest@$mqCommit).** It was built against that commit; with a MacroQuest built from another, it may fail to load.`n`nCopy MQ2Posse.dll into MacroQuest's plugins folder, then ``/plugin MQ2Posse`` on every client. Every client must run this same build. MQ2Posse.pdb is only for debugging crashes."

if ($DryRun) {
    Write-Host "release: dry run OK. Would tag $tag and publish $($assets -join ', ') with notes:`n$notes" -ForegroundColor Green
    exit 0
}

Run git @('tag', '-a', $tag, '-m', "MQ2Posse $version")
Run git @('push', 'origin', $tag)
Run gh @('release', 'create', $tag, $assets[0], $assets[1], '--title', "MQ2Posse $version", '--notes', $notes, '--generate-notes')
Write-Host "release: published $tag" -ForegroundColor Green
