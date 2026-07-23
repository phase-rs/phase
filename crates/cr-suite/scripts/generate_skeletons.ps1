# Thin wrapper around the single-source-of-truth Rust generator.
#
# The Rust binary (`cargo cr-suite --generate`) is the ONLY skeleton generator.
# This script used to re-implement the CompRules walk in PowerShell, which drifted
# from the Rust logic (issue #6514 review). It now simply shells out to cargo so
# there is exactly one generator. It fails loudly if cargo is unavailable or if
# zero rules were parsed (which would indicate a broken CompRules path).

param(
  [Parameter(Mandatory = $true)][string]$RulesPath,
  [Parameter(Mandatory = $true)][string]$OutRoot
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $RulesPath)) {
  Write-Error "CompRules file not found: $RulesPath"
  exit 1
}

# Invoke the Rust generator (preserves authored non-skeleton fixtures via --update).
$output = & cargo run -p cr-suite --bin cr-suite -- `
  --generate --update `
  --comp-rules $RulesPath `
  --scenarios-dir $OutRoot 2>&1
$code = $LASTEXITCODE

Write-Host $output

if ($code -ne 0) {
  Write-Error "cargo cr-suite --generate failed with exit code $code"
  exit $code
}

# Fail loudly if the generator reported that it saw zero rules — a silent
# "saw 0 rules" would previously have written nothing and looked like success.
if ($output -match 'saw\s+0\s+rules') {
  Write-Error "generator parsed 0 rules from $RulesPath — check the CompRules path/format"
  exit 1
}
