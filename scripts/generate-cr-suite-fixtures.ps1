# Regenerates skeleton fixtures from CompRules.txt.
# Prefer `cargo cr-suite --generate --update` when MSVC linking works.
# This script is a Windows fallback mirroring crates/cr-suite/src/catalog.rs.

$ErrorActionPreference = "Stop"
$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$rulesPath = Join-Path $repoRoot "docs/MagicCompRules.txt"
$outRoot = Join-Path $repoRoot "crates/cr-suite/scenarios"
$gen = Join-Path $repoRoot "crates/cr-suite/scripts/generate_skeletons.ps1"

if (-not (Test-Path $rulesPath)) {
  throw "Missing $rulesPath — run ./scripts/fetch-comp-rules.sh first"
}

& $gen -RulesPath $rulesPath -OutRoot $outRoot
