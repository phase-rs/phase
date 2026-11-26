/**
 * R3-F1 regression (PR #11352): a production-generated fixed-count exile
 * activation must traverse the real WASM boundary
 * `get_viewer_transition_snapshot_js`.
 *
 * Witness: Psychatog {1}{U}{B}, Creature — Atog 1/2 (Scryfall oracle id
 * d7c4da85-d1a5-4a27-812d-65d8ee1494dd), verbatim Oracle text:
 * "Discard a card: This creature gets +1/+1 until end of turn.
 * Exile two cards from your graveyard: This creature gets +1/+1 until end of
 * turn." CR 602.1a: "The activation cost is everything before the colon."
 *
 * The `psychatog` record below is production parser output, not hand-shaped
 * AST: `parse_oracle_text` over that verbatim text, assembled into a CardFace
 * ({1}{U}{B}, Creature Atog 1/2) with the export pipeline's `synthesize_all`,
 * validated through `CardDatabase::from_json_str` — the same load path as
 * WASM `load_card_database`. The game then plays a public line (cast,
 * discard x2, exile-2 activation) and asserts the resulting AbilityActivated
 * event — carrying `activation_cost: Cost(Exile { count: 2 })` — crosses the
 * real serde-wasm-bindgen boundary, which dispatches safe JS integers through
 * `visit_i64` (locked serde-wasm-bindgen 0.6.5 `src/de.rs:299-304`).
 *
 * Discrimination: reverting `CountVisitor::visit_i64` makes the direct
 * endpoint call reject with INVALID_TRANSITION_EVENTS and the production
 * projection throw instead of resolving fresh. Hostile counts (-1, 2^32)
 * stay rejected at the same boundary.
 *
 * Run (from the repo root, after building the engines):
 *   ./scripts/build-wasm.sh wasm-dev   # (or the PowerShell equivalent)
 *   cd client && pnpm exec vitest run --config vitest.integration.config.ts \
 *     --coverage.enabled=false src/adapter/__tests__/psychatogExileCountWasm.integration.test.ts
 * CI runs the explicit list at .github/workflows/ci.yml (maintainer-owned);
 * this file needs adding there.
 */
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { afterEach, beforeAll, expect, it, vi } from "vitest";
import init, { clear_game_state } from "@wasm/engine";
import { FakeDataConnection } from "../../network/__tests__/fakeDataConnection";
import type { P2PMessage } from "../../network/protocol";
import type { TransportConnection, TransportPeer } from "../../network/transport";
import { FORMAT_DEFAULTS } from "../../stores/multiplayerStore";
import { P2PGuestAdapter, P2PHostAdapter } from "../p2p-adapter";
import type { GameAction, GameEvent, GameState, ObjectId } from "../types";
import { WasmAdapter } from "../wasm-adapter";

const psychatog = {
  name: "Psychatog",
  mana_cost: { type: "Cost", shards: ["Blue", "Black"], generic: 1 },
  card_type: { supertypes: [], core_types: ["Creature"], subtypes: ["Atog"] },
  power: { type: "Fixed", value: 1 },
  toughness: { type: "Fixed", value: 2 },
  loyalty: null,
  defense: null,
  oracle_text:
    "Discard a card: This creature gets +1/+1 until end of turn.\nExile two cards from your graveyard: This creature gets +1/+1 until end of turn.",
  non_ability_text: null,
  flavor_name: null,
  keywords: [],
  abilities: [
    {
      kind: "Activated",
      effect: {
        type: "Pump",
        power: { type: "Fixed", value: 1 },
        toughness: { type: "Fixed", value: 1 },
        target: { type: "SelfRef" },
      },
      cost: {
        type: "Discard",
        count: { type: "Fixed", value: 1 },
        filter: null,
        random: false,
        self_ref: false,
      },
      sub_ability: null,
      duration: "UntilEndOfTurn",
      description: "Discard a card: ~ gets +1/+1 until end of turn.",
      target_prompt: null,
      condition: null,
      optional_targeting: false,
      optional: false,
      forward_result: false,
    },
    {
      kind: "Activated",
      effect: {
        type: "Pump",
        power: { type: "Fixed", value: 1 },
        toughness: { type: "Fixed", value: 1 },
        target: { type: "SelfRef" },
      },
      cost: { type: "Exile", count: 2, zone: "Graveyard", filter: null },
      sub_ability: null,
      duration: "UntilEndOfTurn",
      description: "Exile two cards from your graveyard: ~ gets +1/+1 until end of turn.",
      target_prompt: null,
      condition: null,
      optional_targeting: false,
      optional: false,
      forward_result: false,
    },
  ],
  triggers: [],
  static_abilities: [],
  replacements: [],
  color_override: null,
  scryfall_oracle_id: "d7c4da85-d1a5-4a27-812d-65d8ee1494dd",
};

function basicLand(name: string, subtype: string, oracleId: string) {
  return {
    name,
    scryfall_oracle_id: oracleId,
    mana_cost: { type: "NoCost" },
    card_type: { supertypes: ["Basic"], core_types: ["Land"], subtypes: [subtype] },
    keywords: [],
    abilities: [],
    triggers: [],
    static_abilities: [],
    replacements: [],
  };
}

const island = basicLand("Island", "Island", "psychatog-regression-island");
const swamp = basicLand("Swamp", "Swamp", "psychatog-regression-swamp");
const forest = basicLand("Forest", "Forest", "psychatog-regression-forest");
const control = {
  ...forest,
  name: "Control",
  scryfall_oracle_id: "uncovered-control",
  card_type: { supertypes: [], core_types: ["Land"], subtypes: [] },
};

// Nine cards: Psychatog plus eight lands. The opening seven plus the turn-two
// and turn-three draws see the whole deck, so Island + Swamp + a third land
// and two discard fodder are in hand by turn three on every seed.
const deck = {
  main_deck: [
    "Psychatog",
    "Island",
    "Island",
    "Island",
    "Swamp",
    "Swamp",
    "Forest",
    "Forest",
    "Forest",
  ],
  sideboard: [],
  commander: [],
};
const opponent = { main_deck: Array<string>(12).fill("Control"), sideboard: [], commander: [] };

class LinkedConnection extends FakeDataConnection {
  readonly peer = "phase-psychatog-test";
  other?: LinkedConnection;

  override send(bytes: unknown) {
    super.send(bytes);
    void this.other?.simulateData(bytes);
  }

  protected override onDecodedSend(_message: P2PMessage) {
    // The other production PeerSession answers keep-alive frames.
  }
}

function connectPair(accept: (connection: TransportConnection) => void) {
  const host = new LinkedConnection();
  const guest = new LinkedConnection();
  host.other = guest;
  guest.other = host;
  accept(host as unknown as TransportConnection);
  return { host, guest: guest as unknown as TransportConnection };
}

beforeAll(async () => {
  const bytes = await readFile(resolve(__dirname, "../../wasm/engine_wasm_bg.wasm"));
  const module = await WebAssembly.compile(bytes);
  expect((await init({ module_or_path: module })).memory).toBeInstanceOf(WebAssembly.Memory);
}, 60_000);

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  clear_game_state();
});

function objectsWhere(state: GameState, predicate: (name: string, zone: string) => boolean): ObjectId[] {
  const found: ObjectId[] = [];
  for (const [id, object] of Object.entries(state.objects)) {
    if (predicate(object.name, object.zone)) found.push(Number(id));
  }
  return found;
}

/** The bound `activation_cost` on a production AbilityActivated event. The
 * client GameEvent type omits it, but the raw engine objects carry it across
 * the WASM boundary untouched — that untouched crossing is what this test
 * proves. */
function boundExileCostOf(event: GameEvent): { count: unknown; zone: unknown } | undefined {
  if (event.type !== "AbilityActivated") return undefined;
  const data = event.data as { activation_cost?: { Cost?: { type?: unknown; count?: unknown; zone?: unknown } } };
  const cost = data.activation_cost?.Cost;
  if (cost?.type !== "Exile") return undefined;
  return { count: cost.count, zone: cost.zone };
}

function isTogActivation(event: GameEvent, togId: ObjectId): boolean {
  return (
    event.type === "AbilityActivated" &&
    (event.data as { source_id?: unknown }).source_id === togId &&
    boundExileCostOf(event) !== undefined
  );
}

it("routes a production Psychatog exile-2 activation through the real WASM transition endpoint", async () => {
  vi.stubGlobal("Worker", class {
    constructor() { throw new Error("force the production main-thread fallback"); }
  });
  const fetcher = vi.fn(async (input: RequestInfo | URL) =>
    String(input) === "/card-data.json"
      ? new Response(JSON.stringify({ psychatog, island, swamp, forest, control }))
      : new Response(null, { status: 404 }),
  );
  vi.stubGlobal("fetch", fetcher);
  const initHost = vi.spyOn(WasmAdapter.prototype, "initializeMultiplayerHostGame");
  const transition = vi.spyOn(WasmAdapter.prototype, "getViewerTransitionSnapshot");
  const peer = { id: "phase-psychatog-host", destroyed: false, disconnected: false,
    on() { return this; }, off() { return this; }, destroy() {},
  } as unknown as TransportPeer;
  let accept!: (connection: TransportConnection) => void;
  const host = new P2PHostAdapter({ player: deck, opponent }, peer, (handler) => {
    accept = handler;
    return () => {};
  }, 2, FORMAT_DEFAULTS.Freeform);
  const projection = vi.spyOn(host as unknown as {
    projectTransitionForViewer(viewer: number, events: GameEvent[], generation: number): Promise<{ fresh: boolean }>;
  }, "projectTransitionForViewer");
  let guest: P2PGuestAdapter | undefined;
  try {
    const joined = vi.fn();
    host.onEvent((event) => {
      if (event.type === "roomFull") joined();
    });
    await host.initialize();
    const initial = connectPair(accept);
    guest = new P2PGuestAdapter({ player: opponent }, peer, peer.id, initial.guest);
    await guest.initialize();
    await vi.waitFor(() => expect(joined).toHaveBeenCalled());
    const random = vi.spyOn(Math, "random").mockReturnValue(4910 / Number.MAX_SAFE_INTEGER);
    try {
      await host.initializeGame();
    } finally {
      random.mockRestore();
    }
    await guest.initializeGame();
    expect(initHost).toHaveBeenCalledOnce();

    const submit = (seat: number, action: GameAction) =>
      seat === 0 ? host.submitAction(action, 0) : guest!.submitAction(action, 1);
    const stateOf = () => host.getState();

    const opening = await stateOf();
    if (opening.waiting_for.type !== "ChooseStartingPlayer") {
      throw new Error(`expected a starting-player choice, got ${opening.waiting_for.type}`);
    }
    await submit(opening.waiting_for.data.player, { type: "ChooseStartingPlayer", data: { player: 0 } });
    for (const seat of [0, 1]) {
      await submit(seat, { type: "MulliganDecision", data: { choice: { type: "Keep" } } });
    }

    // Play lands (Island, then Swamp, then anything) and cast Psychatog.
    // Priority, combat declarations, and cleanup discards are all public
    // prompts; anything else here is a wrong turn in the script.
    let togId: ObjectId | undefined;
    for (let i = 0; i < 200; i++) {
      const state = await stateOf();
      const onField = objectsWhere(state, (name, zone) => name === "Psychatog" && zone === "Battlefield");
      if (onField.length > 0) {
        togId = onField[0];
        break;
      }
      const waiting = state.waiting_for;
      if (waiting.type === "Priority") {
        const player = waiting.data.player;
        if (player !== 0) {
          await submit(player, { type: "PassPriority" });
          continue;
        }
        const legal = await host.getLegalActions();
        const fieldNames = new Set(
          objectsWhere(state, (_name, zone) => zone === "Battlefield").map((id) => state.objects[id].name),
        );
        const need = fieldNames.has("Island") ? (fieldNames.has("Swamp") ? undefined : "Swamp") : "Island";
        const lands = legal.actions.filter((action) => action.type === "PlayLand");
        const pick = need !== undefined
          ? (lands.find((action) => action.type === "PlayLand" && state.objects[action.data.object_id]?.name === need) ?? lands[0])
          : lands[0];
        if (pick) {
          await submit(0, pick);
          continue;
        }
        const cast = legal.actions.find(
          (action) => action.type === "CastSpell" && state.objects[action.data.object_id]?.name === "Psychatog",
        );
        if (cast) {
          await submit(0, cast);
          continue;
        }
        await submit(0, { type: "PassPriority" });
      } else if (waiting.type === "DeclareAttackers") {
        await submit(waiting.data.player, { type: "DeclareAttackers", data: { attacks: [] } });
      } else if (waiting.type === "DeclareBlockers") {
        await submit(waiting.data.player, { type: "DeclareBlockers", data: { assignments: [] } });
      } else if (waiting.type === "DiscardToHandSize") {
        await submit(waiting.data.player, {
          type: "SelectCards",
          data: { cards: waiting.data.cards.slice(0, waiting.data.count) },
        });
      } else {
        throw new Error(`unexpected prompt on the way to Psychatog: ${waiting.type}`);
      }
    }
    if (togId === undefined) throw new Error("Psychatog never reached the battlefield");

    // Ability indices are positions in the object's serialized abilities.
    const field = await stateOf();
    const tog = field.objects[togId];
    const discardIdx = tog.abilities.findIndex((ability) => ability.cost?.type === "Discard");
    const exileIdx = tog.abilities.findIndex((ability) => ability.cost?.type === "Exile");
    expect(discardIdx).toBeGreaterThanOrEqual(0);
    expect(exileIdx).toBeGreaterThanOrEqual(0);
    expect(exileIdx).not.toBe(discardIdx);

    const drainStack = async () => {
      for (let i = 0; i < 50; i++) {
        const drained = await stateOf();
        if (drained.stack.length === 0) return;
        const waiting = drained.waiting_for;
        if (waiting.type !== "Priority") {
          throw new Error(`unexpected prompt while draining the stack: ${waiting.type}`);
        }
        await submit(waiting.data.player, { type: "PassPriority" });
      }
      throw new Error("the stack did not drain");
    };

    // Discard twice through the public activation: two cards reach the graveyard.
    const hand = field.players[0].hand;
    expect(hand.length).toBeGreaterThanOrEqual(2);
    const [fodderA, fodderB] = hand;
    for (const fodder of [fodderA, fodderB]) {
      await submit(0, {
        type: "ActivateAbility",
        data: { source_id: togId, ability_index: discardIdx, payment_mode: { type: "Auto" } },
      });
      const pending = await stateOf();
      if (pending.waiting_for.type !== "PayCost") {
        throw new Error(`expected the discard cost prompt, got ${pending.waiting_for.type}`);
      }
      expect(pending.waiting_for.data.player).toBe(0);
      expect(pending.waiting_for.data.count).toBe(1);
      expect(pending.waiting_for.data.choices).toContain(fodder);
      await submit(0, { type: "SelectCards", data: { cards: [fodder] } });
      await drainStack();
      expect((await stateOf()).objects[fodder].zone).toBe("Graveyard");
    }

    // The exile-2 activation: capture its production events.
    await submit(0, {
      type: "ActivateAbility",
      data: { source_id: togId, ability_index: exileIdx, payment_mode: { type: "Auto" } },
    });
    const costPrompt = await stateOf();
    if (costPrompt.waiting_for.type !== "PayCost") {
      throw new Error(`expected the exile cost prompt, got ${costPrompt.waiting_for.type}`);
    }
    expect(costPrompt.waiting_for.data.player).toBe(0);
    expect(costPrompt.waiting_for.data.count).toBe(2);
    expect(costPrompt.waiting_for.data.choices).toEqual(expect.arrayContaining([fodderA, fodderB]));
    const activation = await submit(0, { type: "SelectCards", data: { cards: [fodderA, fodderB] } });
    const activationEvents = activation.events;
    await drainStack();
    const done = await stateOf();
    expect(done.objects[fodderA].zone).toBe("Exile");
    expect(done.objects[fodderB].zone).toBe("Exile");

    // (a) The production event carries the fixed Exile count. Without this
    // binding the traversal assertions below would prove nothing.
    const activated = activationEvents.find((event) => isTogActivation(event, togId));
    expect(activated).toBeDefined();
    expect(boundExileCostOf(activated!)?.count).toBe(2);
    expect(boundExileCostOf(activated!)?.zone).toBe("Graveyard");

    // (b) The production P2P path fed these events through the real endpoint,
    // and every such projection resolved fresh.
    const sawActivation = transition.mock.calls.some(([, events]) =>
      events.some((event) => isTogActivation(event, togId)),
    );
    expect(sawActivation).toBe(true);
    let covered = false;
    for (const [index, call] of projection.mock.calls.entries()) {
      if (!call[1].some((event) => isTogActivation(event, togId))) continue;
      covered = true;
      const settled = projection.mock.results[index];
      expect(settled?.type).toBe("return");
      if (settled?.type === "return") expect((await settled.value).fresh).toBe(true);
    }
    expect(covered).toBe(true);

    // The same production events traverse the endpoint on direct call.
    const wasm = initHost.mock.contexts[0] as WasmAdapter;
    const direct = await wasm.getViewerTransitionSnapshot(1, activationEvents);
    expect(direct.state.players).toHaveLength(2);

    // The guest observed the exile: the R2 failure mode skipped this update.
    await vi.waitFor(async () => {
      const guestState = await guest!.getState();
      expect(guestState.objects[fodderA].zone).toBe("Exile");
      expect(guestState.objects[fodderB].zone).toBe("Exile");
    });

    // (c) Hostile counts stay rejected at the same boundary.
    for (const hostile of [-1, 2 ** 32]) {
      const tampered = structuredClone(activationEvents);
      const target = tampered.find((event) => isTogActivation(event, togId));
      expect(target).toBeDefined();
      const cost = (
        target as unknown as { data: { activation_cost: { Cost: { count: number } } } }
      ).data.activation_cost.Cost;
      cost.count = hostile;
      await expect(wasm.getViewerTransitionSnapshot(1, tampered)).rejects.toThrow(
        "INVALID_TRANSITION_EVENTS",
      );
    }
  } finally {
    guest?.dispose();
    host.dispose();
  }
}, 120_000);
