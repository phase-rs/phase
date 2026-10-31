import type { ChallengeDeck, ChallengeDeckMetadata, FormatConfig } from "../adapter/types";

// The Rust engine at crates/engine/src/types/format.rs is the canonical source
// of truth for this list; the `getChallengeDeckRegistry` WASM export emits the
// same shape. This file mirrors that registry so React components can render the
// Horde deck picker synchronously before the WASM module loads. A verification
// test compares this constant to the WASM output to catch drift between the two.
//
// Adding a Horde deck is an engine-side change (a `ChallengeDeck` variant plus a
// `metadata` arm); this mirror and its drift test are the only client-side
// follow-up.
export const CHALLENGE_DECK_REGISTRY: readonly ChallengeDeckMetadata[] = [
  {
    deck: "CybermanHorde",
    label: "Cyberman Horde",
    short_label: "CYB",
    description:
      "Doctor Who — Cybermen and Daleks swarm in waves that run until a nontoken card is cast",
    default_ruleset: {
      challenge_deck: "CybermanHorde",
      wave: { type: "UntilNonToken", data: { count: { type: "Fixed", data: 1 } } },
      survivor_setup_turns: 3,
      combined_base_life: 100,
      per_extra_survivor_life_delta: -15,
      horde_creatures_forced_attackers: true,
      legendary_death: "Normal",
      post_combat_activation: "None",
      survivor_deck_format: "Constructed",
    },
    resolved_config: {
      "format": "Horde",
      "starting_life": 100,
      "min_players": 2,
      "max_players": 5,
      "deck_size": {
        "type": "Minimum",
        "data": 60
      },
      "singleton": false,
      "command_zone": true,
      "commander_damage_threshold": null,
      "range_of_influence": null,
      "team_based": false,
      "archenemy_player": 0,
      "uses_commander": false,
      "supplies_fixed_deck": false,
      "sideboard_policy": {
        "type": "Forbidden"
      },
      "default_deck_copy_limit": {
        "type": "Unlimited"
      },
      "allow_debug_actions": false,
      "horde_ruleset": {
        "challenge_deck": "CybermanHorde",
        "wave": {
          "type": "UntilNonToken",
          "data": {
            "count": {
              "type": "Fixed",
              "data": 1
            }
          }
        },
        "survivor_setup_turns": 3,
        "combined_base_life": 100,
        "per_extra_survivor_life_delta": -15,
        "horde_creatures_forced_attackers": true,
        "legendary_death": "Normal",
        "post_combat_activation": "None",
        "survivor_deck_format": "Constructed"
      }
    },
    rules: [
      { label: "Waves", detail: "Reveals until the first nontoken card is cast" },
      { label: "Survivor life", detail: "100 life shared, -15 per extra survivor" },
      { label: "Survivor decks", detail: "Survivors bring 60-card constructed decks" },
      {
        label: "Setup",
        detail: "Survivors take 3 turns to set up before the Horde's first turn",
      },
      {
        label: "Horde combat",
        detail:
          "The Horde's creatures attack every combat if able, and only its Defenders can block",
      },
    ],
  },
  {
    deck: "DndHorde",
    label: "D&D Horde — Oozes",
    short_label: "DND",
    description:
      "Dungeons & Dragons — a self-replicating Ooze swarm; each wave ends when an uncommon or better is revealed",
    default_ruleset: {
      challenge_deck: "DndHorde",
      wave: { type: "UntilRarityAtLeast", data: "uncommon" },
      survivor_setup_turns: 3,
      combined_base_life: 100,
      per_extra_survivor_life_delta: -15,
      horde_creatures_forced_attackers: true,
      legendary_death: "Normal",
      post_combat_activation: "None",
      survivor_deck_format: "Constructed",
    },
    resolved_config: {
      "format": "Horde",
      "starting_life": 100,
      "min_players": 2,
      "max_players": 5,
      "deck_size": {
        "type": "Minimum",
        "data": 60
      },
      "singleton": false,
      "command_zone": true,
      "commander_damage_threshold": null,
      "range_of_influence": null,
      "team_based": false,
      "archenemy_player": 0,
      "uses_commander": false,
      "supplies_fixed_deck": false,
      "sideboard_policy": {
        "type": "Forbidden"
      },
      "default_deck_copy_limit": {
        "type": "Unlimited"
      },
      "allow_debug_actions": false,
      "horde_ruleset": {
        "challenge_deck": "DndHorde",
        "wave": {
          "type": "UntilRarityAtLeast",
          "data": "uncommon"
        },
        "survivor_setup_turns": 3,
        "combined_base_life": 100,
        "per_extra_survivor_life_delta": -15,
        "horde_creatures_forced_attackers": true,
        "legendary_death": "Normal",
        "post_combat_activation": "None",
        "survivor_deck_format": "Constructed"
      }
    },
    rules: [
      { label: "Waves", detail: "A wave ends at the first uncommon or better card" },
      { label: "Survivor life", detail: "100 life shared, -15 per extra survivor" },
      { label: "Survivor decks", detail: "Survivors bring 60-card constructed decks" },
      {
        label: "Setup",
        detail: "Survivors take 3 turns to set up before the Horde's first turn",
      },
      {
        label: "Horde combat",
        detail:
          "The Horde's creatures attack every combat if able, and only its Defenders can block",
      },
    ],
  },
  {
    deck: "ZombiesHorde",
    label: "Zombies Horde",
    short_label: "ZOM",
    description:
      "An undead swarm whose waves ramp 1 → 2 → 3 nontokens and back down, snaking between pressure and respite",
    default_ruleset: {
      challenge_deck: "ZombiesHorde",
      wave: {
        type: "UntilNonToken",
        data: { count: { type: "Snaking", data: { min: 1, max: 3 } } },
      },
      survivor_setup_turns: 3,
      combined_base_life: 100,
      per_extra_survivor_life_delta: -15,
      horde_creatures_forced_attackers: true,
      legendary_death: "Normal",
      post_combat_activation: "None",
      survivor_deck_format: "Constructed",
    },
    resolved_config: {
      "format": "Horde",
      "starting_life": 100,
      "min_players": 2,
      "max_players": 5,
      "deck_size": {
        "type": "Minimum",
        "data": 60
      },
      "singleton": false,
      "command_zone": true,
      "commander_damage_threshold": null,
      "range_of_influence": null,
      "team_based": false,
      "archenemy_player": 0,
      "uses_commander": false,
      "supplies_fixed_deck": false,
      "sideboard_policy": {
        "type": "Forbidden"
      },
      "default_deck_copy_limit": {
        "type": "Unlimited"
      },
      "allow_debug_actions": false,
      "horde_ruleset": {
        "challenge_deck": "ZombiesHorde",
        "wave": {
          "type": "UntilNonToken",
          "data": {
            "count": {
              "type": "Snaking",
              "data": {
                "min": 1,
                "max": 3
              }
            }
          }
        },
        "survivor_setup_turns": 3,
        "combined_base_life": 100,
        "per_extra_survivor_life_delta": -15,
        "horde_creatures_forced_attackers": true,
        "legendary_death": "Normal",
        "post_combat_activation": "None",
        "survivor_deck_format": "Constructed"
      }
    },
    rules: [
      {
        label: "Waves",
        detail: "Reveals until N nontokens are cast — N snakes 1 → 3 → 1 each turn",
      },
      { label: "Survivor life", detail: "100 life shared, -15 per extra survivor" },
      { label: "Survivor decks", detail: "Survivors bring 60-card constructed decks" },
      {
        label: "Setup",
        detail: "Survivors take 3 turns to set up before the Horde's first turn",
      },
      {
        label: "Horde combat",
        detail:
          "The Horde's creatures attack every combat if able, and only its Defenders can block",
      },
    ],
  },
  {
    deck: "SliversHorde",
    label: "Slivers Horde",
    short_label: "SLV",
    description:
      "Every Sliver buffs every other Sliver — the swarm compounds, growing stronger as it grows wider",
    default_ruleset: {
      challenge_deck: "SliversHorde",
      wave: { type: "UntilRarityAtLeast", data: "uncommon" },
      survivor_setup_turns: 3,
      combined_base_life: 100,
      per_extra_survivor_life_delta: -15,
      horde_creatures_forced_attackers: true,
      legendary_death: "Normal",
      post_combat_activation: "None",
      survivor_deck_format: "Constructed",
    },
    resolved_config: {
      "format": "Horde",
      "starting_life": 100,
      "min_players": 2,
      "max_players": 5,
      "deck_size": {
        "type": "Minimum",
        "data": 60
      },
      "singleton": false,
      "command_zone": true,
      "commander_damage_threshold": null,
      "range_of_influence": null,
      "team_based": false,
      "archenemy_player": 0,
      "uses_commander": false,
      "supplies_fixed_deck": false,
      "sideboard_policy": {
        "type": "Forbidden"
      },
      "default_deck_copy_limit": {
        "type": "Unlimited"
      },
      "allow_debug_actions": false,
      "horde_ruleset": {
        "challenge_deck": "SliversHorde",
        "wave": {
          "type": "UntilRarityAtLeast",
          "data": "uncommon"
        },
        "survivor_setup_turns": 3,
        "combined_base_life": 100,
        "per_extra_survivor_life_delta": -15,
        "horde_creatures_forced_attackers": true,
        "legendary_death": "Normal",
        "post_combat_activation": "None",
        "survivor_deck_format": "Constructed"
      }
    },
    rules: [
      { label: "Waves", detail: "A wave ends at the first uncommon or better card" },
      { label: "Survivor life", detail: "100 life shared, -15 per extra survivor" },
      { label: "Survivor decks", detail: "Survivors bring 60-card constructed decks" },
      {
        label: "Setup",
        detail: "Survivors take 3 turns to set up before the Horde's first turn",
      },
      {
        label: "Horde combat",
        detail:
          "The Horde's creatures attack every combat if able, and only its Defenders can block",
      },
    ],
  },
  {
    deck: "HumansGodzillaHorde",
    label: "Humans & Godzilla Horde",
    short_label: "HGZ",
    description:
      "A wide, cheap human army punctuated by a handful of enormous Godzilla-series titans",
    default_ruleset: {
      challenge_deck: "HumansGodzillaHorde",
      wave: { type: "UntilRarityAtLeast", data: "uncommon" },
      survivor_setup_turns: 3,
      combined_base_life: 100,
      per_extra_survivor_life_delta: -15,
      horde_creatures_forced_attackers: true,
      legendary_death: "Normal",
      post_combat_activation: "None",
      survivor_deck_format: "Constructed",
    },
    resolved_config: {
      "format": "Horde",
      "starting_life": 100,
      "min_players": 2,
      "max_players": 5,
      "deck_size": {
        "type": "Minimum",
        "data": 60
      },
      "singleton": false,
      "command_zone": true,
      "commander_damage_threshold": null,
      "range_of_influence": null,
      "team_based": false,
      "archenemy_player": 0,
      "uses_commander": false,
      "supplies_fixed_deck": false,
      "sideboard_policy": {
        "type": "Forbidden"
      },
      "default_deck_copy_limit": {
        "type": "Unlimited"
      },
      "allow_debug_actions": false,
      "horde_ruleset": {
        "challenge_deck": "HumansGodzillaHorde",
        "wave": {
          "type": "UntilRarityAtLeast",
          "data": "uncommon"
        },
        "survivor_setup_turns": 3,
        "combined_base_life": 100,
        "per_extra_survivor_life_delta": -15,
        "horde_creatures_forced_attackers": true,
        "legendary_death": "Normal",
        "post_combat_activation": "None",
        "survivor_deck_format": "Constructed"
      }
    },
    rules: [
      { label: "Waves", detail: "A wave ends at the first uncommon or better card" },
      { label: "Survivor life", detail: "100 life shared, -15 per extra survivor" },
      { label: "Survivor decks", detail: "Survivors bring 60-card constructed decks" },
      {
        label: "Setup",
        detail: "Survivors take 3 turns to set up before the Horde's first turn",
      },
      {
        label: "Horde combat",
        detail:
          "The Horde's creatures attack every combat if able, and only its Defenders can block",
      },
    ],
  },
  {
    deck: "SauronHorde",
    label: "Sauron, the Dark Lord Horde",
    short_label: "SAU",
    description:
      "The Lord of the Rings “Two Towers” — Sauron's near-all-creature swarm of Orcs, Nazgûl, and siege beasts; revealed Orc Armies amass one growing army, and milled legendaries recur by phasing out",
    default_ruleset: {
      challenge_deck: "SauronHorde",
      wave: { type: "UntilRarityAtLeast", data: "uncommon" },
      survivor_setup_turns: 3,
      combined_base_life: 100,
      per_extra_survivor_life_delta: -15,
      horde_creatures_forced_attackers: true,
      legendary_death: "EtbThenPhaseOut",
      post_combat_activation: "None",
      survivor_deck_format: "Commander",
    },
    resolved_config: {
      "format": "Horde",
      "starting_life": 100,
      "min_players": 2,
      "max_players": 5,
      "deck_size": {
        "type": "Exactly",
        "data": 100
      },
      "singleton": true,
      "command_zone": true,
      "commander_damage_threshold": 21,
      "range_of_influence": null,
      "team_based": false,
      "archenemy_player": 0,
      "uses_commander": true,
      "supplies_fixed_deck": false,
      "sideboard_policy": {
        "type": "Forbidden"
      },
      "default_deck_copy_limit": {
        "type": "UpTo",
        "data": 1
      },
      "allow_debug_actions": false,
      "horde_ruleset": {
        "challenge_deck": "SauronHorde",
        "wave": {
          "type": "UntilRarityAtLeast",
          "data": "uncommon"
        },
        "survivor_setup_turns": 3,
        "combined_base_life": 100,
        "per_extra_survivor_life_delta": -15,
        "horde_creatures_forced_attackers": true,
        "legendary_death": "EtbThenPhaseOut",
        "post_combat_activation": "None",
        "survivor_deck_format": "Commander"
      }
    },
    rules: [
      { label: "Waves", detail: "A wave ends at the first uncommon or better card" },
      { label: "Survivor life", detail: "100 life shared, -15 per extra survivor" },
      {
        label: "Survivor decks",
        detail: "Survivors bring 100-card singleton Commander (EDH) decks",
      },
      {
        label: "Setup",
        detail: "Survivors take 3 turns to set up before the Horde's first turn",
      },
      {
        label: "Horde combat",
        detail:
          "The Horde's creatures attack every combat if able, and only its Defenders can block",
      },
      {
        label: "Legendary deaths",
        detail:
          "A milled legendary enters, triggers its ETB, then phases out — it returns on the Horde's next untap instead of being removed",
      },
    ],
  },
  {
    deck: "SarumanHorde",
    label: "Saruman, the White Hand Horde",
    short_label: "SAR",
    description:
      "The Lord of the Rings “Two Towers” — Saruman's Uruk-hai and Orcs backed by removal and enchantments that grow the White Hand's Orc Army; uncommon+ waves and legendaries that recur by phasing out",
    default_ruleset: {
      challenge_deck: "SarumanHorde",
      wave: { type: "UntilRarityAtLeast", data: "uncommon" },
      survivor_setup_turns: 3,
      combined_base_life: 100,
      per_extra_survivor_life_delta: -15,
      horde_creatures_forced_attackers: true,
      legendary_death: "EtbThenPhaseOut",
      post_combat_activation: "None",
      survivor_deck_format: "Commander",
    },
    resolved_config: {
      "format": "Horde",
      "starting_life": 100,
      "min_players": 2,
      "max_players": 5,
      "deck_size": {
        "type": "Exactly",
        "data": 100
      },
      "singleton": true,
      "command_zone": true,
      "commander_damage_threshold": 21,
      "range_of_influence": null,
      "team_based": false,
      "archenemy_player": 0,
      "uses_commander": true,
      "supplies_fixed_deck": false,
      "sideboard_policy": {
        "type": "Forbidden"
      },
      "default_deck_copy_limit": {
        "type": "UpTo",
        "data": 1
      },
      "allow_debug_actions": false,
      "horde_ruleset": {
        "challenge_deck": "SarumanHorde",
        "wave": {
          "type": "UntilRarityAtLeast",
          "data": "uncommon"
        },
        "survivor_setup_turns": 3,
        "combined_base_life": 100,
        "per_extra_survivor_life_delta": -15,
        "horde_creatures_forced_attackers": true,
        "legendary_death": "EtbThenPhaseOut",
        "post_combat_activation": "None",
        "survivor_deck_format": "Commander"
      }
    },
    rules: [
      { label: "Waves", detail: "A wave ends at the first uncommon or better card" },
      { label: "Survivor life", detail: "100 life shared, -15 per extra survivor" },
      {
        label: "Survivor decks",
        detail: "Survivors bring 100-card singleton Commander (EDH) decks",
      },
      {
        label: "Setup",
        detail: "Survivors take 3 turns to set up before the Horde's first turn",
      },
      {
        label: "Horde combat",
        detail:
          "The Horde's creatures attack every combat if able, and only its Defenders can block",
      },
      {
        label: "Legendary deaths",
        detail:
          "A milled legendary enters, triggers its ETB, then phases out — it returns on the Horde's next untap instead of being removed",
      },
    ],
  },
  {
    deck: "LotrTwoTowersHorde",
    label: "LOTR: The Two Towers Horde",
    short_label: "2TW",
    description:
      "The Lord of the Rings “Two Towers” — the full two-Horde experience: Sauron and Saruman as two commanders, each with its own deck, battlefield, and graveyard, ALTERNATING turns against the survivor team",
    default_ruleset: {
      challenge_deck: "LotrTwoTowersHorde",
      co_horde_decks: ["SarumanHorde"],
      wave: { type: "UntilRarityAtLeast", data: "uncommon" },
      survivor_setup_turns: 3,
      combined_base_life: 100,
      per_extra_survivor_life_delta: -15,
      horde_creatures_forced_attackers: true,
      legendary_death: "EtbThenPhaseOut",
      post_combat_activation: "None",
      survivor_deck_format: "Commander",
    },
    resolved_config: {
      "format": "Horde",
      "starting_life": 100,
      "min_players": 2,
      "max_players": 5,
      "deck_size": {
        "type": "Exactly",
        "data": 100
      },
      "singleton": true,
      "command_zone": true,
      "commander_damage_threshold": 21,
      "range_of_influence": null,
      "team_based": false,
      "archenemy_player": 0,
      "uses_commander": true,
      "supplies_fixed_deck": false,
      "sideboard_policy": {
        "type": "Forbidden"
      },
      "default_deck_copy_limit": {
        "type": "UpTo",
        "data": 1
      },
      "allow_debug_actions": false,
      "horde_ruleset": {
        "challenge_deck": "LotrTwoTowersHorde",
        "wave": {
          "type": "UntilRarityAtLeast",
          "data": "uncommon"
        },
        "survivor_setup_turns": 3,
        "combined_base_life": 100,
        "per_extra_survivor_life_delta": -15,
        "horde_creatures_forced_attackers": true,
        "legendary_death": "EtbThenPhaseOut",
        "post_combat_activation": "None",
        "survivor_deck_format": "Commander",
        "co_horde_decks": [
          "SarumanHorde"
        ]
      }
    },
    rules: [
      { label: "Waves", detail: "A wave ends at the first uncommon or better card" },
      { label: "Survivor life", detail: "100 life shared, -15 per extra survivor" },
      {
        label: "Survivor decks",
        detail: "Survivors bring 100-card singleton Commander (EDH) decks",
      },
      {
        label: "Setup",
        detail: "Survivors take 3 turns to set up before the Horde's first turn",
      },
      {
        label: "Horde combat",
        detail:
          "The Horde's creatures attack every combat if able, and only its Defenders can block",
      },
      {
        label: "Legendary deaths",
        detail:
          "A milled legendary enters, triggers its ETB, then phases out — it returns on the Horde's next untap instead of being removed",
      },
    ],
  },
  {
    deck: "TurtleTeamUpHorde",
    label: "Turtle Team-Up Horde",
    short_label: "TMT",
    description:
      "Teenage Mutant Ninja Turtles — staged Bosses sharing one life total per stage, backed by an event deck of Ninjas and villain Events",
    default_ruleset: {
      challenge_deck: "TurtleTeamUpHorde",
      wave: { type: "FixedCount", data: 2 },
      survivor_setup_turns: 0,
      combined_base_life: 10,
      per_extra_survivor_life_delta: 10,
      horde_creatures_forced_attackers: true,
      legendary_death: "Normal",
      post_combat_activation: "None",
      survivor_deck_format: "Constructed",
      boss_stages: {
        stage_sizes: [1, 2, 3],
        life_base: 20,
        life_at_four_survivors: 30,
      },
    },
    resolved_config: {
      "format": "Horde",
      "starting_life": 100,
      "min_players": 2,
      "max_players": 5,
      "deck_size": {
        "type": "Minimum",
        "data": 60
      },
      "singleton": false,
      "command_zone": true,
      "commander_damage_threshold": null,
      "range_of_influence": null,
      "team_based": false,
      "archenemy_player": 0,
      "uses_commander": false,
      "supplies_fixed_deck": false,
      "sideboard_policy": {
        "type": "Forbidden"
      },
      "default_deck_copy_limit": {
        "type": "Unlimited"
      },
      "allow_debug_actions": false,
      "horde_ruleset": {
        "challenge_deck": "TurtleTeamUpHorde",
        "wave": {
          "type": "FixedCount",
          "data": 2
        },
        "survivor_setup_turns": 0,
        "combined_base_life": 10,
        "per_extra_survivor_life_delta": 10,
        "horde_creatures_forced_attackers": true,
        "legendary_death": "Normal",
        "post_combat_activation": "None",
        "survivor_deck_format": "Constructed",
        "boss_stages": {
          "stage_sizes": [
            1,
            2,
            3
          ],
          "life_base": 20,
          "life_at_four_survivors": 30
        }
      }
    },
    rules: [
      { label: "Waves", detail: "Reveals 2 cards each Horde turn" },
      { label: "Survivor life", detail: "10 life shared, +10 per extra survivor" },
      {
        label: "Survivor decks",
        detail: "Survivors bring 60-card constructed decks",
      },
      {
        label: "Setup",
        detail: "Survivors take 0 turns to set up before the Horde's first turn",
      },
      {
        label: "Horde combat",
        detail:
          "The Horde's creatures attack every combat if able, and only its Defenders can block",
      },
      {
        label: "Bosses",
        detail:
          "Bosses deploy in 3 stages (1 → 2 → 3) sharing 20 life per stage (30 with 4+ heroes)",
      },
    ],
  },
];

/** The deck a Horde game uses when the player hasn't picked one. */
export const DEFAULT_CHALLENGE_DECK: ChallengeDeck = CHALLENGE_DECK_REGISTRY[0].deck;

export function challengeDeckMetadata(
  deck: ChallengeDeck,
): ChallengeDeckMetadata | undefined {
  return CHALLENGE_DECK_REGISTRY.find((m) => m.deck === deck);
}

/**
 * Minimum table size for a Horde game with the picked challenge deck: one
 * survivor plus the Horde roster (the primary plus one seat per co-Horde
 * deck, from the deck's engine-resolved ruleset). The engine rejects smaller
 * tables (`validate_for_player_count`), so setup bumps the player count to
 * this when the pick needs more seats. Unknown deck ⇒ the single-Horde
 * floor of 2.
 */
export function hordeMinimumPlayers(deck: ChallengeDeck): number {
  const meta = challengeDeckMetadata(deck);
  const coHordes = meta?.resolved_config?.horde_ruleset?.co_horde_decks?.length ?? 0;
  return 2 + coHordes;
}

/**
 * Apply a chosen challenge deck to a Horde `FormatConfig`.
 *
 * The engine owns the per-deck rules, so this applies the deck's whole
 * engine-resolved configuration — the `HordeRuleset` (wave policy, setup
 * turns, life deltas) AND the survivor deck-shape axes (`deck_size`,
 * `singleton`, commander fields) derived from it by `FormatConfig::horde` —
 * rather than mutating `challenge_deck` alone. Picking a deck must leave
 * neither another deck's wave rule nor its EDH/constructed shape attached.
 * Setup-screen edits (`starting_life`), the transport remap
 * (`archenemy_player`), and host seat choices (`min/max_players`) stay.
 * Returns `config` untouched for non-Horde formats or an unknown deck.
 */
export function withChallengeDeck(
  config: FormatConfig,
  deck: ChallengeDeck,
): FormatConfig {
  if (config.format !== "Horde") return config;
  const meta = challengeDeckMetadata(deck);
  if (!meta) return config;
  const resolved = meta.resolved_config;
  return {
    ...config,
    horde_ruleset: meta.default_ruleset,
    deck_size: resolved.deck_size,
    singleton: resolved.singleton,
    uses_commander: resolved.uses_commander,
    commander_damage_threshold: resolved.commander_damage_threshold,
    default_deck_copy_limit: resolved.default_deck_copy_limit,
  };
}
