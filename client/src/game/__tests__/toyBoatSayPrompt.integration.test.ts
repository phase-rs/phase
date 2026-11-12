/**
 * Real-WASM witness for the Toy Boat timed spoken payment.
 *
 * The unit tests cover the panel and the native Rust tests cover the engine,
 * but both can agree while the built browser exports disagree: the opaque
 * interaction projection is what the browser actually submits, and the receipt
 * stamp is what makes a saying timely. This file drives the real built exports
 * end to end — it projects the prompt twice, submits the engine-authored
 * `SayPhrase` choice with a host timestamp, then checks the engine's progress
 * and settlement.
 *
 * Manual integration lane: no CI job builds the WASM binary. Regenerate the
 * fixture with
 * `cargo test -p engine-wasm --lib hosted_takeback_tests::write_toy_boat_say_prompt_fixture -- --ignored --exact`
 * after `./scripts/build-wasm.sh`, then run:
 * `cd client && pnpm exec vitest run --config vitest.integration.config.ts \
 *   --coverage.enabled=false \
 *   src/game/__tests__/toyBoatSayPrompt.integration.test.ts`
 */
import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";

import { beforeAll, describe, expect, it } from "vitest";

import init, {
  get_viewer_snapshot_js,
  load_card_database,
  restore_game_state,
  submit_interaction_js,
} from "@wasm/engine";

const WASM_JS = resolve(__dirname, "../../wasm/engine_wasm.js");
const WASM_BINARY = resolve(__dirname, "../../wasm/engine_wasm_bg.wasm");
const FIXTURE = resolve(__dirname, "fixtures/toyBoatSayPrompt.json");
const INPUTS_PRESENT =
  existsSync(WASM_JS) && existsSync(WASM_BINARY) && existsSync(FIXTURE);

type PresentationSurface = {
  type: string;
  data?: { code?: string };
};

type InteractionChoice = {
  id: string;
  surfaces: PresentationSurface[];
};

type InteractionOpportunity = {
  interactionId: string;
  response: {
    type: string;
    data?: { choices?: InteractionChoice[] };
  };
};

/**
 * The raw export is wider than the adapter's `ViewerSnapshot`: `state` is the
 * engine's `ClientGameStateRef` envelope (`{ state, derived }`), which
 * `wasm-adapter.ts` unwraps before consumers see it. This test drives the
 * export directly, so it unwraps that envelope itself.
 */
type RawViewerSnapshot = {
  state: {
    state: {
      say_payment?: {
        said?: number;
        last_said_ms?: number;
        last_seq?: number;
      } | null;
      waiting_for?: { type?: string };
      objects?: Record<string, { name?: string; zone?: string }>;
    };
  };
  viewerInteraction: {
    opportunities: InteractionOpportunity[];
  };
};

function gameStateOf(snapshot: RawViewerSnapshot) {
  return snapshot.state.state;
}

type SubmitOutcome = {
  status: string;
  result?: { events?: unknown[] };
};

/**
 * The engine's `Action` presentation surface names the exact action a choice
 * materializes, so the test selects the saying through the wire contract
 * instead of guessing an index. `InteractionActionCode::SayPhrase` serializes
 * as `sayPhrase`.
 */
function sayingChoice(snapshot: RawViewerSnapshot): InteractionChoice {
  const opportunity = snapshot.viewerInteraction.opportunities[0];
  expect(opportunity, "the restored state projects one live opportunity").toBeDefined();
  expect(opportunity.response.type).toBe("exactChoices");
  const choices = opportunity.response.data?.choices ?? [];
  const choice = choices.find((candidate) =>
    candidate.surfaces.some(
      (surface) => surface.type === "action" && surface.data?.code === "sayPhrase",
    ),
  );
  expect(choice, "the engine projects the SayPhrase choice").toBeDefined();
  return choice!;
}

function submissionFor(snapshot: RawViewerSnapshot) {
  const opportunity = snapshot.viewerInteraction.opportunities[0];
  return {
    interactionId: opportunity.interactionId,
    response: {
      type: "choose",
      data: { choiceId: sayingChoice(snapshot).id },
    },
  };
}

describe.skipIf(!INPUTS_PRESENT)(
  "Toy Boat timed spoken payment through the built WASM exports",
  () => {
    beforeAll(async () => {
      const module = await WebAssembly.compile(await readFile(WASM_BINARY));
      await init({ module_or_path: module });
      // The restore rehydrates printed metadata from the card database. The
      // exported fixture already carries Toy Boat's materialized battlefield
      // state, so an empty database is enough to install it.
      load_card_database("{}");
    });

    it("projects deterministically, submits stamped sayings, and settles", async () => {
      restore_game_state(await readFile(FIXTURE, "utf8"));

      const first = get_viewer_snapshot_js(0) as RawViewerSnapshot;
      const second = get_viewer_snapshot_js(0) as RawViewerSnapshot;
      expect(second).toEqual(first);
      expect(gameStateOf(first).say_payment).toMatchObject({ said: 0, owed: 2 });

      const firstOutcome = submit_interaction_js(
        0,
        submissionFor(first),
        10_000,
      ) as SubmitOutcome;
      expect(firstOutcome.status).toBe("applied");

      const progress = get_viewer_snapshot_js(0) as RawViewerSnapshot;
      expect(gameStateOf(progress).say_payment).toMatchObject({
        said: 1,
        last_said_ms: 10_000,
        last_seq: 1,
      });

      const secondOutcome = submit_interaction_js(
        0,
        submissionFor(progress),
        10_400,
      ) as SubmitOutcome;
      expect(secondOutcome.status).toBe("applied");

      const settled = get_viewer_snapshot_js(0) as RawViewerSnapshot;
      const settledState = gameStateOf(settled);
      expect(settledState.say_payment).toBeUndefined();
      expect(settledState.waiting_for?.type).not.toBe("UnlessPayment");
      expect(
        Object.values(settledState.objects ?? {}).some(
          (object) => object.name === "Toy Boat" && object.zone === "Battlefield",
        ),
      ).toBe(true);
    });
  },
);
