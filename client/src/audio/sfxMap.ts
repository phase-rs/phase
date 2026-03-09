/**
 * Maps GameEvent type strings to Forge SFX filenames (without extension).
 * Only maps to confirmed-existing files from Forge's res/sound/ directory.
 */
export const SFX_MAP: Record<string, string> = {
  DamageDealt: "destroy",
  LifeChanged: "life_loss",
  SpellCast: "instant",
  CreatureDestroyed: "destroy",
  AttackersDeclared: "creature",
  BlockersDeclared: "block",
  LandPlayed: "green_land",
  CardDrawn: "draw",
  SpellCountered: "sorcery",
  TokenCreated: "token",
  GameStarted: "shuffle",
  GameOver: "win_duel",
  PermanentSacrificed: "destroy",
  CounterAdded: "add_counter",
  AbilityActivated: "enchant",
};

/** Forge battle music tracks (Kevin MacLeod, CC-BY 3.0). */
export const MUSIC_TRACKS: string[] = [
  "Dangerous",
  "Failing Defense",
  "Hitman",
  "Prelude and Action",
];
