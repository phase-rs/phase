/**
 * Maps GameEvent type strings to SFX filenames (without extension).
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

/** Battle music tracks (Kevin MacLeod, CC-BY 3.0). */
export const MUSIC_TRACKS: string[] = [
  "Dangerous",
  "Failing Defense",
  "Hitman",
  "Prelude and Action",
];
