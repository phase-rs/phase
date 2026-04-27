import type { GameState, PlayerId, Zone } from "../adapter/types";
import { restoreGameState } from "../game/dispatch";
import { getCardFaceData } from "../services/engineRuntime";
import { useGameStore } from "../stores/gameStore";

/**
 * Dev-only helper: insert a fresh GameObject for a named card into a zone,
 * then round-trip through the engine's `restore_game_state` so the
 * `rehydrate_game_from_card_db` pass fills in abilities/triggers/statics from
 * the card database.
 *
 * Uses an existing GameObject from the current state as a structural template
 * — guarantees every Rust-side serde-required field is present at the right
 * shape. Only differentiating fields (id, name, zone, printed_ref, runtime
 * counters/flags) get overwritten before submission.
 *
 * Strictly a development affordance — wired only into DebugPanel. Not safe in
 * multiplayer (the existing `restoreGameState` path is local/AI-only).
 */
export async function injectCard(
  cardName: string,
  owner: PlayerId,
  zone: Zone,
  count = 1,
): Promise<{ ok: true; added: number } | { ok: false; error: string }> {
  const { gameState } = useGameStore.getState();
  if (!gameState) return { ok: false, error: "no active game state" };

  const face = (await getCardFaceData(cardName).catch(() => null)) as
    | {
        name: string;
        scryfall_oracle_id?: string | null;
        card_type: unknown;
        mana_cost: unknown;
        power?: string | null;
        toughness?: string | null;
        loyalty?: string | null;
      }
    | null;

  if (!face || typeof face !== "object" || !face.name) {
    return { ok: false, error: `card "${cardName}" not in database` };
  }
  if (!face.scryfall_oracle_id) {
    return { ok: false, error: `card "${cardName}" has no oracle id (cannot rehydrate)` };
  }

  const templateKey = Object.keys(gameState.objects)[0];
  if (templateKey == null) {
    return { ok: false, error: "no existing objects to use as template" };
  }
  const template = gameState.objects[Number(templateKey)];

  const updated: GameState = {
    ...gameState,
    objects: { ...gameState.objects },
    players: gameState.players.map((p) => ({ ...p })),
    battlefield: [...gameState.battlefield],
    next_object_id: gameState.next_object_id,
  };

  const player = updated.players.find((p) => p.id === owner);
  if (!player) return { ok: false, error: `player ${owner} not in game` };

  for (let i = 0; i < count; i++) {
    const id = updated.next_object_id;
    const stub = JSON.parse(JSON.stringify(template));

    stub.id = id;
    stub.card_id = id;
    stub.owner = owner;
    stub.controller = owner;
    stub.zone = zone;
    stub.name = face.name;
    stub.tapped = false;
    stub.face_down = false;
    stub.flipped = false;
    stub.transformed = false;
    stub.damage_marked = 0;
    stub.dealt_deathtouch_damage = false;
    stub.attached_to = null;
    stub.attachments = [];
    stub.counters = {};
    stub.entered_battlefield_turn = null;
    stub.summoning_sick = false;
    stub.has_summoning_sickness = false;
    stub.has_mana_ability = false;
    stub.mana_ability_index = undefined;
    stub.printed_ref = { oracle_id: face.scryfall_oracle_id, face_name: face.name };

    // Provisional shape — `rehydrate_game_from_card_db` overwrites these from
    // the parsed CardFace inside `restore_game_state`. Setting them avoids
    // a brief render with template-leaked fields if the UI rerenders before
    // restore completes.
    stub.card_types = face.card_type;
    stub.mana_cost = face.mana_cost;
    stub.power = parsePT(face.power);
    stub.toughness = parsePT(face.toughness);
    stub.loyalty = parsePT(face.loyalty);
    stub.abilities = [];
    stub.trigger_definitions = [];
    stub.static_definitions = [];
    stub.replacement_definitions = [];
    stub.keywords = [];

    updated.objects[id] = stub;
    updated.next_object_id = id + 1;

    if (zone === "Battlefield") {
      updated.battlefield.push(id);
    } else if (zone === "Hand") {
      player.hand = [...player.hand, id];
    } else if (zone === "Graveyard") {
      player.graveyard = [...player.graveyard, id];
    } else if (zone === "Library") {
      player.library = [...player.library, id];
    } else {
      return { ok: false, error: `unsupported zone: ${zone}` };
    }
  }

  const err = await restoreGameState(updated);
  if (err) return { ok: false, error: err };
  return { ok: true, added: count };
}

function parsePT(value: string | null | undefined): number | null {
  if (value == null) return null;
  const n = parseInt(value, 10);
  return Number.isNaN(n) ? null : n;
}
