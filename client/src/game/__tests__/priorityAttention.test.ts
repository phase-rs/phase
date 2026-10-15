import { describe, expect, it } from "vitest";

import {
  acknowledgeInput,
  announceHiddenWindow,
  ESCALATE_AFTER_MS,
  escalateAttention,
  escalationDeadline,
  initialAttentionMachine,
  attentionCue,
  MIN_WAIT_BEFORE_ALERT_MS,
  stepAttention as stepWithVisibility,
  windowKey,
  type AttentionMachine,
  type AttentionObservation,
} from "../priorityAttention";
import { buildGameState, buildStackEntry } from "../../test/factories/gameStateFactory";

/** The visible-tab step most cases use; the hidden-tab cases pass it explicitly. */
const stepAttention = (
  m: AttentionMachine,
  observation: AttentionObservation,
  now: number,
  visibility: DocumentVisibilityState = "visible",
) => stepWithVisibility(m, observation, now, visibility);

const isAnnouncing = (m: AttentionMachine) => attentionCue(m) !== "none";
const isEscalated = (m: AttentionMachine) => attentionCue(m) === "reminded";

const reactive = (key = "w1"): AttentionObservation => ({ kind: "reactive", key });
const planning = (key = "w1"): AttentionObservation => ({ kind: "planning", key });
const engaged: AttentionObservation = { kind: "engaged" };
const away: AttentionObservation = { kind: "away" };

/** Run a sequence of [observation, time] steps from a fresh machine at t=0. */
function run(...steps: [AttentionObservation, number][]): AttentionMachine {
  return steps.reduce(
    (machine, [observation, now]) => stepAttention(machine, observation, now),
    initialAttentionMachine(0),
  );
}

describe("announcing a window", () => {
  it("announces a window that opens after being away long enough", () => {
    const m = run([reactive(), MIN_WAIT_BEFORE_ALERT_MS]);
    expect(m.fireCount).toBe(1);
    expect(isAnnouncing(m)).toBe(true);
  });

  it("draws the line at exactly MIN_WAIT_BEFORE_ALERT_MS", () => {
    for (const [awayFor, announced] of [
      [MIN_WAIT_BEFORE_ALERT_MS - 1, false],
      [MIN_WAIT_BEFORE_ALERT_MS, true],
    ] as const) {
      const m = run([planning(), awayFor]);
      expect(isAnnouncing(m)).toBe(announced);
      expect(m.fireCount).toBe(announced ? 1 : 0);
    }
  });

  it("measures the wait from when the previous window closed", () => {
    let m = run([reactive(), 10_000]); // announced: away since load
    m = stepAttention(m, away, 10_100); // the player passed; the idle clock restarts
    m = stepAttention(m, reactive("w2"), 10_100 + MIN_WAIT_BEFORE_ALERT_MS - 1);
    expect(m.fireCount).toBe(1);
    m = stepAttention(m, away, 20_000);
    m = stepAttention(m, reactive("w3"), 20_000 + MIN_WAIT_BEFORE_ALERT_MS);
    expect(m.fireCount).toBe(2);
  });

  it("does not restart the idle clock while already away", () => {
    const m = run([away, 2_000], [away, 3_000], [planning(), MIN_WAIT_BEFORE_ALERT_MS]);
    expect(m.fireCount).toBe(1);
  });

  it("neither clears nor re-announces the same window re-committed", () => {
    let m = run([reactive(), 5_000]);
    for (const t of [6_000, 7_000, 60_000]) m = stepAttention(m, reactive(), t);
    expect(m.fireCount).toBe(1);
    expect(isAnnouncing(m)).toBe(true);
  });
});

describe("the player's own decisions", () => {
  it("do not count as time away", () => {
    let m = run([engaged, 1_000]);
    m = stepAttention(m, engaged, 1_000 + MIN_WAIT_BEFORE_ALERT_MS * 3);
    m = stepAttention(m, planning(), 1_000 + MIN_WAIT_BEFORE_ALERT_MS * 3 + 50);
    expect(m.fireCount).toBe(0);
    expect(isAnnouncing(m)).toBe(false);
  });

  it("still announce after a genuine wait that follows them", () => {
    const m = run([engaged, 1_000], [away, 2_000], [reactive(), 2_000 + MIN_WAIT_BEFORE_ALERT_MS]);
    expect(m.fireCount).toBe(1);
  });

  it("close a held window when a decision of their own opens", () => {
    let m = run([reactive(), MIN_WAIT_BEFORE_ALERT_MS]);
    expect(isAnnouncing(m)).toBe(true);
    m = stepAttention(m, engaged, MIN_WAIT_BEFORE_ALERT_MS + 500);
    expect(isAnnouncing(m)).toBe(false);
    m = stepAttention(m, reactive("w2"), MIN_WAIT_BEFORE_ALERT_MS + 900);
    expect(m.fireCount).toBe(1);
  });
});

describe("player input", () => {
  it("answers an announced window and keeps it answered across re-commits", () => {
    let m = run([planning(), MIN_WAIT_BEFORE_ALERT_MS]);
    m = acknowledgeInput(m, MIN_WAIT_BEFORE_ALERT_MS + 100);
    expect(isAnnouncing(m)).toBe(false);
    m = stepAttention(m, planning(), MIN_WAIT_BEFORE_ALERT_MS + 1_000);
    expect(isAnnouncing(m)).toBe(false);
    expect(m.fireCount).toBe(1);
  });

  it("restarts the idle clock when no window is held (a prompt with no single owner)", () => {
    // A mulligan reads as away; the player is at the screen, and says so by clicking Keep.
    let m = run([away, 1_000]);
    m = acknowledgeInput(m, MIN_WAIT_BEFORE_ALERT_MS * 3);
    m = stepAttention(m, planning(), MIN_WAIT_BEFORE_ALERT_MS * 3 + 1_000);
    expect(m.fireCount).toBe(0);
    expect(isAnnouncing(m)).toBe(false);
  });

  it("is a no-op while the player is engaged", () => {
    const m = run([engaged, 1_000]);
    expect(acknowledgeInput(m, 2_000)).toBe(m);
  });

  it("does not suppress a window that opens after a genuine wait following the input", () => {
    let m = run([away, 0]);
    m = acknowledgeInput(m, 1_000);
    m = stepAttention(m, planning(), 1_000 + MIN_WAIT_BEFORE_ALERT_MS);
    expect(isAnnouncing(m)).toBe(true);
  });
});

describe("the reminder", () => {
  it("never applies to a planning window", () => {
    const m = run([planning(), MIN_WAIT_BEFORE_ALERT_MS]);
    expect(isAnnouncing(m)).toBe(true);
    expect(escalationDeadline(m)).toBeNull();
    expect(escalateAttention(m)).toBe(m);
  });

  it("is due ESCALATE_AFTER_MS after a reactive window opened", () => {
    const m = run([reactive(), MIN_WAIT_BEFORE_ALERT_MS]);
    expect(escalationDeadline(m)).toBe(MIN_WAIT_BEFORE_ALERT_MS + ESCALATE_AFTER_MS);
    expect(isEscalated(m)).toBe(false);
    const reminded = escalateAttention(m);
    expect(isEscalated(reminded)).toBe(true);
    expect(reminded.fireCount).toBe(1); // a reminder is not another announcement
  });

  it("still reaches a reactive window that opened too soon to announce, without a chime", () => {
    const m = run([reactive(), 1_000]);
    expect(isAnnouncing(m)).toBe(false);
    expect(m.fireCount).toBe(0);
    expect(escalationDeadline(m)).toBe(1_000 + ESCALATE_AFTER_MS);

    const reminded = escalateAttention(m);
    expect(isAnnouncing(reminded)).toBe(true); // glow and title, no chime
    expect(isEscalated(reminded)).toBe(true);
    expect(reminded.fireCount).toBe(0);
  });

  it("is dropped once the window is answered", () => {
    const m = acknowledgeInput(run([reactive(), 1_000]), 2_000);
    expect(escalationDeadline(m)).toBeNull();
    expect(escalateAttention(m)).toBe(m);
  });

  it("is cancelled when the window becomes planning, and re-anchored when it turns reactive", () => {
    let m = escalateAttention(run([reactive(), MIN_WAIT_BEFORE_ALERT_MS]));
    m = stepAttention(m, planning(), 30_000);
    expect(isEscalated(m)).toBe(false);
    expect(isAnnouncing(m)).toBe(true);
    expect(escalationDeadline(m)).toBeNull();
    expect(m.fireCount).toBe(1);

    m = stepAttention(m, reactive(), 40_000);
    expect(escalationDeadline(m)).toBe(40_000 + ESCALATE_AFTER_MS);
    expect(isEscalated(m)).toBe(false);
  });

  it("does not resurrect an answer when the kind changes", () => {
    let m = acknowledgeInput(run([reactive(), MIN_WAIT_BEFORE_ALERT_MS]), 5_000);
    m = stepAttention(m, planning(), 6_000);
    expect(isAnnouncing(m)).toBe(false);
  });
});

describe("a different window after the player answered", () => {
  it("is a fresh, quiet window with its own reminder, not a continuation of the answered one", () => {
    // Window A announced and answered (a Pass click); the engine auto-passes the
    // other seats and hands priority back at the next step with no away commit.
    let m = acknowledgeInput(run([reactive("A"), MIN_WAIT_BEFORE_ALERT_MS]), 5_000);
    m = stepAttention(m, reactive("B"), 6_000);

    expect(isAnnouncing(m)).toBe(false); // quiet: they were just here
    expect(m.fireCount).toBe(1); // and no second chime
    expect(escalationDeadline(m)).toBe(6_000 + ESCALATE_AFTER_MS);
    expect(isAnnouncing(escalateAttention(m))).toBe(true);
  });

  it("keeps an unanswered announcement when the window changes under the player", () => {
    let m = run([reactive("A"), MIN_WAIT_BEFORE_ALERT_MS]);
    m = stepAttention(m, reactive("B"), MIN_WAIT_BEFORE_ALERT_MS + 2_000);
    expect(isAnnouncing(m)).toBe(true);
    expect(m.fireCount).toBe(1);
    expect(escalationDeadline(m)).toBe(MIN_WAIT_BEFORE_ALERT_MS + 2_000 + ESCALATE_AFTER_MS);
  });

  it("restarts a reminder that had already fired", () => {
    let m = escalateAttention(run([reactive("A"), MIN_WAIT_BEFORE_ALERT_MS]));
    m = stepAttention(m, reactive("B"), 30_000);
    expect(isEscalated(m)).toBe(false);
    expect(isAnnouncing(m)).toBe(true);
  });
});

describe("windowKey", () => {
  // Every row starts from the same baseline and changes exactly one axis, so
  // dropping that component from the key makes its row fail.
  const baseline = () =>
    buildGameState({
      turn_number: 5,
      phase: "CombatDamage",
      stack: [buildStackEntry({ id: 70 })],
      steps_started_this_turn: { CombatDamage: 1, PostCombatMain: 1 },
    });
  const keyOf = (over: Parameters<typeof buildGameState>[0], seat = 0) =>
    windowKey({ ...baseline(), ...over }, seat);

  it("is equal for a re-read of the same snapshot", () => {
    expect(windowKey(baseline(), 0)).toBe(windowKey(baseline(), 0));
  });

  it.each([
    ["the turn", keyOf({ turn_number: 6 })],
    ["the step", keyOf({ phase: "PostCombatMain" })],
    ["the occurrence of the step (the second combat damage step)", keyOf({ steps_started_this_turn: { CombatDamage: 2 } })],
    ["the seat holding semantic priority", keyOf({}, 2)],
    ["the stack entries at the same depth", keyOf({ stack: [buildStackEntry({ id: 71 })] })],
    ["the stack depth", keyOf({ stack: [buildStackEntry({ id: 70 }), buildStackEntry({ id: 72 })] })],
  ] as const)("changes with %s", (_what, other) => {
    expect(other).not.toBe(windowKey(baseline(), 0));
  });

  it("treats an absent step tally like an empty one", () => {
    const { steps_started_this_turn: _omit, ...rest } = baseline();
    expect(windowKey(rest as ReturnType<typeof baseline>, 0)).toBe(
      windowKey({ ...baseline(), steps_started_this_turn: {} }, 0),
    );
  });
});

describe("a hidden tab", () => {
  it.each([["reactive", reactive()], ["planning", planning()]] as const)(
    "announces a %s window that opens sooner than the away gate",
    (_kind, observation) => {
      const m = stepAttention(initialAttentionMachine(0), observation, 1_000, "hidden");
      expect(isAnnouncing(m)).toBe(true);
      expect(m.fireCount).toBe(1);
    },
  );

  it("announces a window held quiet when the tab becomes hidden, once", () => {
    const quiet = run([planning(), 1_000]);
    expect(isAnnouncing(quiet)).toBe(false);

    const announced = announceHiddenWindow(quiet);
    expect(isAnnouncing(announced)).toBe(true);
    expect(announced.fireCount).toBe(1);
    expect(announceHiddenWindow(announced)).toBe(announced);
  });

  it("changes nothing for an answered window, a held-none state, or a window already shown", () => {
    const answered = acknowledgeInput(run([planning(), 1_000]), 2_000);
    expect(announceHiddenWindow(answered)).toBe(answered);
    const away0 = initialAttentionMachine(0);
    expect(announceHiddenWindow(away0)).toBe(away0);
  });
});

describe("a hidden tab and a window that replaces an answered one", () => {
  const answeredA = (kind: "reactive" | "planning") =>
    acknowledgeInput(
      run([kind === "reactive" ? reactive("A") : planning("A"), MIN_WAIT_BEFORE_ALERT_MS]),
      5_000,
    );

  it.each(["reactive", "planning"] as const)(
    "announces a %s replacement window that opens while hidden",
    (kind) => {
      const next = stepAttention(answeredA(kind), kind === "reactive" ? reactive("B") : planning("B"), 6_000, "hidden");
      expect(isAnnouncing(next)).toBe(true);
      expect(next.fireCount).toBe(2);
    },
  );

  it("leaves the answered window answered, and the replacement quiet while visible", () => {
    const a = answeredA("planning");
    expect(stepAttention(a, planning("A"), 6_000, "hidden")).toBe(a);
    const quiet = stepAttention(a, planning("B"), 6_000, "visible");
    expect(isAnnouncing(quiet)).toBe(false);
    expect(quiet.fireCount).toBe(1);
  });

  it("does not announce the replacement again on a re-commit, nor re-chime a carried announcement", () => {
    let m = stepAttention(answeredA("planning"), planning("B"), 6_000, "hidden");
    expect(m.fireCount).toBe(2);
    m = stepAttention(m, planning("B"), 7_000, "hidden");
    expect(m.fireCount).toBe(2);

    // An announcement the player has not answered carries over to the next window.
    const unanswered = run([planning("A"), MIN_WAIT_BEFORE_ALERT_MS]);
    const carried = stepAttention(unanswered, planning("B"), 6_000, "hidden");
    expect(isAnnouncing(carried)).toBe(true);
    expect(carried.fireCount).toBe(1);
  });

  it("starts a restarted machine from the count it is given", () => {
    const m = initialAttentionMachine(0, 7);
    expect(stepAttention(m, planning(), MIN_WAIT_BEFORE_ALERT_MS).fireCount).toBe(8);
  });
});
