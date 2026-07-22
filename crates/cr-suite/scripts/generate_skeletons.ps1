# Regenerates skeleton fixtures. Prefer: cargo cr-suite --generate --update
# This PowerShell copy exists for environments without a working MSVC linker.

param(
  [Parameter(Mandatory = $true)][string]$RulesPath,
  [Parameter(Mandatory = $true)][string]$OutRoot
)

$ErrorActionPreference = "Stop"

$included = @(
  100,101,102,103,104,105,106,107,108,109,110,111,112,113,114,115,116,117,118,119,120,121,122,
  200,201,202,204,205,207,208,
  300,301,302,303,304,305,306,307,308,310,
  400,401,402,403,404,405,406,407,408,
  500,501,502,503,504,505,506,507,508,509,510,511,512,513,514,
  600,601,602,603,604,605,606,607,608,609,610,611,612,613,614,615,616,
  700,701,702,703,704,705,706,707,708,709,710,711,712,713,714,715,716,717,718,719,720,721,722,723,724,725,726,727,728,729,730,731,732,
  800,903
)

$titles = @{
  100="General";101="The Magic Golden Rules";102="Players";103="Starting the Game";104="Ending the Game"
  105="Colors";106="Mana";107="Numbers and Symbols";108="Cards";109="Objects";110="Permanents";111="Tokens"
  112="Spells";113="Abilities";114="Emblems";115="Targets";116="Special Actions";117="Timing and Priority"
  118="Costs";119="Life";120="Damage";121="Drawing a Card";122="Counters";200="General (Parts of a Card)"
  201="Name";202="Mana Cost and Color";204="Mana Value";205="Type Line";207="Text Box";208="Power/Toughness"
  300="General (Card Types)";301="Artifacts";302="Creatures";303="Enchantments";304="Instants";305="Lands"
  306="Planeswalkers";307="Sorceries";308="Tribals";310="Battles";400="General (Zones)";401="Library"
  402="Hand";403="Battlefield";404="Graveyard";405="Stack";406="Exile";407="Ante";408="Command"
  500="General (Turn Structure)";501="Beginning Phase";502="Untap Step";503="Upkeep Step";504="Draw Step"
  505="Main Phase";506="Combat Phase";507="Beginning of Combat Step";508="Declare Attackers Step"
  509="Declare Blockers Step";510="Combat Damage Step";511="End of Combat Step";512="Ending Phase"
  513="End Step";514="Cleanup Step";600="General (Spells, Abilities, and Effects)";601="Casting Spells"
  602="Activating Activated Abilities";603="Handling Triggered Abilities";604="Handling Static Abilities"
  605="Mana Abilities";606="Loyalty Abilities";607="Linked Abilities";608="Resolving Spells and Abilities"
  609="Effects";610="One-Shot Effects";611="Continuous Effects";612="Text-Changing Effects"
  613="Interaction of Continuous Effects";614="Replacement Effects";615="Prevention Effects"
  616="Interaction of Replacement and/or Prevention Effects";700="General (Additional Rules)"
  701="Keyword Actions";702="Keyword Abilities";703="Turn-Based Actions";704="State-Based Actions"
  705="Flipping a Coin";706="Rolling a Die";707="Copying Objects";708="Face-Down Spells and Permanents"
  709="Split Cards";710="Flip Cards";711="Leveler Cards";712="Double-Faced Cards";713="Substitute Cards"
  714="Saga Cards";715="Adventurer Cards";716="Class Cards";717="Attraction Cards";718="Prototype Cards"
  719="Case Cards";720="Taking Shortcuts";721="Handling Illegal Actions";722="Ending Turns and Phases"
  723="The Monarch";724="The Initiative";725="The Ring Tempts You";726="Restarting the Game";727="Subgames"
  728="Merging with Permanents";729="Daybound and Nightbound";730="Miscellaneous";731="Controlling Another Player"
  732="Ending the Turn";800="General (Multiplayer Rules)";903="Commander"
}

function Escape-Toml([string]$s) {
  return ($s -replace '\\', '\\' -replace '"', '\"')
}

$content = Get-Content -Path $RulesPath -Raw
$lines = $content -split "`n"
$pastToc = $false
$tocCount = 0
$seen = [System.Collections.Generic.HashSet[string]]::new()
$written = 0
$preserved = 0

foreach ($line in $lines) {
  $trimmed = $line.Trim()
  if ([string]::IsNullOrEmpty($trimmed)) { continue }
  if ($trimmed -eq '100. General') {
    $tocCount++
    if ($tocCount -ge 2) { $pastToc = $true }
    continue
  }
  if (-not $pastToc) { continue }
  if ($trimmed -notmatch '^(\d{3}\.\d+[a-z]?)\b') { continue }
  $number = $Matches[1]
  $section = [int]$number.Substring(0, 3)
  if ($included -notcontains $section) { continue }
  if (-not $seen.Add($number)) { continue }

  $text = $trimmed.Substring($number.Length).Trim().TrimStart('.').Trim()
  $stem = 'cr_' + ($number -replace '\.', '_')
  $dir = Join-Path $OutRoot ('{0:D3}' -f $section)
  New-Item -ItemType Directory -Force -Path $dir | Out-Null
  $path = Join-Path $dir ($stem + '.toml')
  if (Test-Path $path) {
    $existing = Get-Content $path -Raw
    if ($existing -match 'status\s*=\s*"(executable|not-applicable|deferred)"') {
      $preserved++
      continue
    }
  }
  $title = if ($titles.ContainsKey($section)) { $titles[$section] } else { 'Unknown' }
  $titleFull = "$title — CR $number"
  @"
rule = "$(Escape-Toml $number)"
section = $section
title = "$(Escape-Toml $titleFull)"
status = "skeleton"
text = "$(Escape-Toml $text)"
"@ | ForEach-Object {
  $utf8NoBom = New-Object System.Text.UTF8Encoding $false
  [System.IO.File]::WriteAllText($path, $_, $utf8NoBom)
}
  $written++
}

Write-Host "wrote=$written preserved=$preserved unique=$($seen.Count)"
