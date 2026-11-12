import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";

import type {
  EngineAdapter,
  EngineSnapshot,
  GameAction,
  GameState,
  LegalActionsResult,
  PhrasePace,
  SayPhrasePayment,
  SubmitResult,
  WaitingFor,
} from "../../adapter/types";
import { nextSnapshotSeq } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import { useMultiplayerStore } from "../../stores/multiplayerStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { useUiStore } from "../../stores/uiStore";
import {
  buildGameState,
  buildLegalActionsResult,
  buildPriorityWaitingFor,
} from "../../test/factories/gameStateFactory";
import { GamePage } from "../GamePage";

vi.mock("../../providers/GameProvider.tsx", () => ({
  GameProvider: ({ children }: { children: React.ReactNode }) => <>{children}</>,
}));
vi.mock("../../game/sessionCleanup.ts", () => ({ clearPromptOverlayState: vi.fn() }));

vi.mock("../../hooks/useCardImage.ts", () => ({
  useCardImage: vi.fn(() => ({
    src: null,
    isLoading: false,
    isRotated: false,
    isFlip: false,
  })),
}));
vi.mock("../../hooks/useIsMobile.ts", () => ({
  useIsMobile: () => false,
  useIsCompactHeight: () => false,
}));
vi.mock("../../audio/useAudioContext.ts", () => ({ useAudioContext: () => undefined }));
vi.mock("../../hooks/useGameplayPreferencesSync.ts", () => ({
  useGameplayPreferencesSync: () => undefined,
}));
vi.mock("../../hooks/useCardDataMeta.ts", () => ({
  useCardDataMeta: () => null,
  formatRelativeDate: () => "",
}));

vi.mock("../../components/board/BattlefieldBackground.tsx", () => ({
  BattlefieldBackground: () => null,
}));
vi.mock("../../components/stack/StackDisplay.tsx", () => ({ StackDisplay: () => null }));
vi.mock("../../components/debug/DebugPanel.tsx", () => ({ DebugPanel: () => null }));
vi.mock("../../components/hud/HUD.tsx", () => ({ HUD: () => null }));
vi.mock("../../components/board/GameBoard.tsx", () => ({ GameBoard: () => null }));
vi.mock("../../components/modal/EngineLostModal.tsx", () => ({ EngineLostModal: () => null }));
vi.mock("../../components/modal/CardDataMissingModal.tsx", () => ({
  CardDataMissingModal: () => null,
}));
vi.mock("../../components/multiplayer/ConcedeDialog.tsx", () => ({
  ConcedeDialog: () => null,
}));
vi.mock("../../components/chrome/GameMenu.tsx", () => ({ GameMenu: () => null }));

vi.mock("../../stores/draftStore.ts", () => ({
  useDraftStore: vi.fn(() => ({
    phase: "idle",
    pool: [],
    picks: [],
    packs: [],
    currentPack: null,
    currentPickIndex: 0,
    draftComplete: false,
  })),
}));
vi.mock("../../services/quickDraftPersistence.ts", () => ({
  loadActiveQuickDraft: vi.fn(() => null),
  saveQuickDraftRun: vi.fn(),
  deleteQuickDraftRun: vi.fn(),
}));
vi.mock("../../adapter/draft-adapter.ts", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../adapter/draft-adapter.ts")>()),
  createDraftAdapter: vi.fn(),
}));

const PHRASE = "Toy Boat";
const PAYMENT_ID = 11;
const OWED = 2;
/** The engine's published pace window; the fake engine validates the gap
 *  between the host stamps the panel sends. */
const GAP_MS = 1_000;

type Saying = { payment: number; seq: number; at_ms: number };

function prompt(owed: number): WaitingFor {
  return {
    type: "UnlessPayment",
    data: {
      player: 0,
      cost: {
        type: "SayPhrase",
        phrase: PHRASE,
        pace: { type: "Quickly", max_gap_ms: GAP_MS } satisfies PhrasePace,
        repetitions: owed,
      },
      pending_effect: {},
      effect_description: "Sacrifice Toy Boat",
    },
  } as unknown as WaitingFor;
}

function payment(said: number): SayPhrasePayment {
  return { id: PAYMENT_ID, owed: OWED, said, clock_ms: 0 };
}

/**
 * A deterministic stand-in for the engine's spoken-payment bookkeeping. It
 * accepts one `SayPhrase` per saying, in sequence, inside the pace window, and
 * exposes the `say_payment` instance and remaining repetitions the real engine
 * publishes. The FIRST saying's reply is held until `releaseFirst()`, modelling
 * a multiplayer round trip that is still in flight when the player's second
 * click lands.
 */
class FakeToyBoatEngine {
  said = 0;
  declined = false;
  readonly submitted: GameAction[] = [];
  private readonly sayings: Saying[] = [];
  private release!: () => void;
  private readonly firstReply = new Promise<void>((resolve) => {
    this.release = resolve;
  });

  private queue = Promise.resolve();

  releaseFirst = () => this.release();

  submittedSayings(): Saying[] {
    return this.submitted
      .filter(
        (action): action is GameAction & { type: "SayPhrase" } =>
          action.type === "SayPhrase" &&
          (action.data as { report?: string })?.report !== "Expiry",
      )
      .map((action) => action.data as Saying);
  }

  async submitAction(action: GameAction): Promise<SubmitResult> {
    this.submitted.push(action);
    return new Promise((resolve) => {
      this.queue = this.queue.then(async () => {
        if (action.type === "PayUnlessCost") {
          if (action.data.pay === false) this.declined = true;
          resolve({ events: [], log_entries: [] });
          return;
        }
        if (action.type !== "SayPhrase") {
          resolve({ events: [], log_entries: [] });
          return;
        }

        const saying = action.data as Saying & { report?: string };
        if (saying.report === "Expiry") {
          resolve({ events: [], log_entries: [] });
          return;
        }

        const sayingCount = this.submitted.filter(
          (s) =>
            s.type === "SayPhrase" &&
            (s.data as { report?: string })?.report !== "Expiry",
        ).length;
        if (sayingCount === 1) {
          await this.firstReply;
        }
        const last = this.sayings[this.sayings.length - 1];
        const valid =
          saying.payment === PAYMENT_ID &&
          saying.seq === this.said + 1 &&
          (last === undefined || saying.at_ms - last.at_ms <= GAP_MS);
        if (valid) {
          this.sayings.push(saying);
          this.said += 1;
        }
        resolve({ events: [], log_entries: [] });
      });
    });
  }

  state(): GameState {
    const done = this.said >= OWED;
    return buildGameState({
      waiting_for: done ? buildPriorityWaitingFor() : prompt(OWED - this.said),
      active_player: 0,
      priority_player: 0,
      turn_number: 3,
      say_payment: done ? null : payment(this.said),
    });
  }

  legalResult(): LegalActionsResult {
    return buildLegalActionsResult({ actions: [] });
  }

  adapter(): EngineAdapter {
    return {
      initialize: vi.fn().mockResolvedValue(undefined),
      initializeGame: vi.fn().mockResolvedValue({ events: [] } as SubmitResult),
      restoreState: vi.fn(),
      estimateBracket: vi.fn().mockResolvedValue(null),
      dispose: vi.fn(),
      submitAction: vi.fn((action: GameAction) => this.submitAction(action)),
      getState: vi.fn(async () => this.state()),
      getLegalActions: vi.fn(async () => this.legalResult()),
      getSnapshot: vi.fn(
        async (): Promise<EngineSnapshot> => ({
          state: this.state(),
          legalResult: this.legalResult(),
          seq: nextSnapshotSeq(),
        }),
      ),
    } as unknown as EngineAdapter;
  }
}

function gamePageTree() {
  return (
    <MemoryRouter initialEntries={["/game/say-phrase?mode=join"]}>
      <Routes>
        <Route path="/game/:id" element={<GamePage />} />
      </Routes>
    </MemoryRouter>
  );
}

function phraseBox() {
  return screen.getByRole("button", { name: new RegExp(PHRASE) });
}

describe("GamePage rapid sayings through the real dispatch queue", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
    usePreferencesStore.setState({ animationSpeedMultiplier: 1 });
    useMultiplayerStore.setState({ activePlayerId: 0, isSpectator: false });
    useUiStore.setState({ pendingAbilityChoice: null, enchantmentsDialogPlayer: null });
  });

  afterEach(() => {
    cleanup();
    act(() => {
      useGameStore.setState({ gameId: null, gameState: null, waitingFor: null, adapter: null });
      useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false });
    });
  });

  it("submits both rapid sayings, completes the payment, and strands no acknowledgement", async () => {
    const engine = new FakeToyBoatEngine();
    const initial = engine.state();
    useGameStore.setState({
      gameId: "say-phrase",
      gameMode: "ai",
      adapter: engine.adapter(),
      gameState: initial,
      waitingFor: initial.waiting_for,
      legalActions: [],
      events: [],
      eventHistory: [],
      logHistory: [],
      nextLogSeq: 0,
      stateHistory: [],
      turnCheckpoints: [],
      lastCommittedSeq: 0,
    });

    render(gamePageTree());
    expect(await screen.findByText(PHRASE)).toBeInTheDocument();

    // Both clicks land while the first submission is in flight.
    // Timed sayings are submitted immediately without awaiting the first reply.
    fireEvent.click(phraseBox());
    fireEvent.click(phraseBox());
    expect(engine.submittedSayings()).toHaveLength(2);

    engine.releaseFirst();
    await waitFor(() =>
      expect(useGameStore.getState().waitingFor?.type).not.toBe("UnlessPayment"),
    );

    const sayings = engine.submittedSayings();
    expect(sayings.map((saying) => saying.seq)).toEqual([1, 2]);
    expect(new Set(sayings.map((saying) => saying.seq)).size).toBe(2);
    // The engine accepted both: the payment completed rather than stranding
    // one saying owed and one acknowledged.
    expect(engine.said).toBe(OWED);
    expect(engine.declined).toBe(false);
    expect(useGameStore.getState().gameState?.say_payment ?? null).toBeNull();
  });

  it("lapses payment when second saying arrives past the pace deadline (late input)", async () => {
    const engine = new FakeToyBoatEngine();
    const originalSubmitAction = engine.submitAction.bind(engine);
    const firstStamp = 1_000;
    engine.submitAction = async (action: GameAction) => {
      if (action.type === "SayPhrase") {
        const seq = (action.data as Saying).seq;
        const stampedAt = seq === 1 ? firstStamp : firstStamp + GAP_MS + 200;
        (action.data as Saying).at_ms = stampedAt;
      }
      return originalSubmitAction(action);
    };

    const initial = engine.state();
    useGameStore.setState({
      gameId: "say-phrase-late",
      gameMode: "ai",
      adapter: engine.adapter(),
      gameState: initial,
      waitingFor: initial.waiting_for,
      legalActions: [],
      events: [],
      eventHistory: [],
      logHistory: [],
      nextLogSeq: 0,
      stateHistory: [],
      turnCheckpoints: [],
      lastCommittedSeq: 0,
    });

    render(gamePageTree());
    expect(await screen.findByText(PHRASE)).toBeInTheDocument();

    fireEvent.click(phraseBox());
    fireEvent.click(phraseBox());
    engine.releaseFirst();

    await waitFor(() => expect(engine.submittedSayings()).toHaveLength(2));
    expect(engine.said).toBe(1);
  });

  it("continues the sequence without replaying seq when reconnecting mid-payment", async () => {
    const engine = new FakeToyBoatEngine();
    engine.said = 1;
    const initial = buildGameState({
      waiting_for: prompt(1),
      active_player: 0,
      priority_player: 0,
      turn_number: 3,
      say_payment: { id: PAYMENT_ID, owed: OWED, said: 1, clock_ms: 0, last_seq: 1 },
    });

    useGameStore.setState({
      gameId: "say-phrase-reconnect",
      gameMode: "ai",
      adapter: engine.adapter(),
      gameState: initial,
      waitingFor: initial.waiting_for,
      legalActions: [],
      events: [],
      eventHistory: [],
      logHistory: [],
      nextLogSeq: 0,
      stateHistory: [],
      turnCheckpoints: [],
      lastCommittedSeq: 0,
    });

    render(gamePageTree());
    expect(await screen.findByText(PHRASE)).toBeInTheDocument();

    fireEvent.click(phraseBox());
    expect(engine.submittedSayings()).toHaveLength(1);
    expect(engine.submittedSayings()[0].seq).toBe(2);
  });
});
