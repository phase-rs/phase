import { describe, expect, it } from "vitest";

import {
  acknowledgeInput,
  ESCALATE_AFTER_MS,
  escalateAttention,
  escalationDeadline,
  initialAttentionMachine,
  attentionCue,
  MIN_WAIT_BEFORE_ALERT_MS,
  noteElsewhere,
  stepAttention as stepWithPresence,
  windowKey,
  type AttentionMachine,
  type AttentionObservation,
  type Presence,
} from "../priorityAttention";
import { buildGameState, buildStackEntry } from "../../test/factories/gameStateFactory";

/** The in-front-of-the-player step most cases use; the elsewhere cases pass it explicitly. */
const stepAttention = (
  m: AttentionMachine,
  observation: AttentionObservation,
  now: number,
  presence: Presence = "attending",
) => stepWithPresence(m, observation, now, presence);

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

describe("announcing a window (glow)", () => {
  it("announces a window that opens after being away long enough, without a chime", () => {
    const m = run([reactive(), MIN_WAIT_BEFORE_ALERT_MS]);
    expect(isAnnouncing(m)).toBe(true);
    expect(m.chimeCount).toBe(0);
  });

  it("draws the line at exactly MIN_WAIT_BEFORE_ALERT_MS", () => {
    for (const [awayFor, announced] of [
      [MIN_WAIT_BEFORE_ALERT_MS - 1, false],
      [MIN_WAIT_BEFORE_ALERT_MS, true],
    ] as const) {
      expect(isAnnouncing(run([planning(), awayFor]))).toBe(announced);
    }
  });

  it("measures the wait from when the previous window closed", () => {
    let m = run([reactive(), 10_000]); // announced: away since load
    m = stepAttention(m, away, 10_100); // the player passed; the idle clock restarts
    m = stepAttention(m, reactive("w2"), 10_100 + MIN_WAIT_BEFORE_ALERT_MS - 1);
    m = acknowledgeInput(m, 10_100 + MIN_WAIT_BEFORE_ALERT_MS); // answered
    expect(isAnnouncing(m)).toBe(false);
    m = stepAttention(m, away, 20_000);
    m = stepAttention(m, reactive("w3"), 20_000 + MIN_WAIT_BEFORE_ALERT_MS);
    expect(isAnnouncing(m)).toBe(true);
  });

  it("does not restart the idle clock while already away", () => {
    const m = run([away, 2_000], [away, 3_000], [planning(), MIN_WAIT_BEFORE_ALERT_MS]);
    expect(isAnnouncing(m)).toBe(true);
  });

  it("neither clears nor re-announces the same window re-committed", () => {
    const first = run([reactive(), 5_000]);
    let m = first;
    for (const t of [6_000, 7_000, 60_000]) m = stepAttention(m, reactive(), t);
    expect(m).toBe(first);
    expect(isAnnouncing(m)).toBe(true);
  });
});

describe("the player's own decisions", () => {
  it("do not count as time away", () => {
    let m = run([engaged, 1_000]);
    m = stepAttention(m, engaged, 1_000 + MIN_WAIT_BEFORE_ALERT_MS * 3);
    m = stepAttention(m, planning(), 1_000 + MIN_WAIT_BEFORE_ALERT_MS * 3 + 50);
    expect(isAnnouncing(m)).toBe(false);
  });

  it("still announce after a genuine wait that follows them", () => {
    const m = run([engaged, 1_000], [away, 2_000], [reactive(), 2_000 + MIN_WAIT_BEFORE_ALERT_MS]);
    expect(isAnnouncing(m)).toBe(true);
  });

  it("close a held window when a decision of their own opens", () => {
    let m = run([reactive(), MIN_WAIT_BEFORE_ALERT_MS]);
    expect(isAnnouncing(m)).toBe(true);
    m = stepAttention(m, engaged, MIN_WAIT_BEFORE_ALERT_MS + 500);
    expect(isAnnouncing(m)).toBe(false);
    m = stepAttention(m, reactive("w2"), MIN_WAIT_BEFORE_ALERT_MS + 900);
    expect(isAnnouncing(m)).toBe(false);
  });
});

describe("player input", () => {
  it("answers an announced window and keeps it answered across re-commits", () => {
    let m = run([planning(), MIN_WAIT_BEFORE_ALERT_MS]);
    m = acknowledgeInput(m, MIN_WAIT_BEFORE_ALERT_MS + 100);
    expect(isAnnouncing(m)).toBe(false);
    m = stepAttention(m, planning(), MIN_WAIT_BEFORE_ALERT_MS + 1_000);
    expect(isAnnouncing(m)).toBe(false);
  });

  it("restarts the idle clock when no window is held (a prompt with no single owner)", () => {
    // A mulligan reads as away; the player is at the screen, and says so by clicking Keep.
    let m = run([away, 1_000]);
    m = acknowledgeInput(m, MIN_WAIT_BEFORE_ALERT_MS * 3);
    m = stepAttention(m, planning(), MIN_WAIT_BEFORE_ALERT_MS * 3 + 1_000);
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
    expect(isEscalated(escalateAttention(m))).toBe(true);
  });

  it("still reaches a reactive window that opened too soon to announce", () => {
    const m = run([reactive(), 1_000]);
    expect(isAnnouncing(m)).toBe(false);
    expect(escalationDeadline(m)).toBe(1_000 + ESCALATE_AFTER_MS);

    const reminded = escalateAttention(m);
    expect(isAnnouncing(reminded)).toBe(true);
    expect(isEscalated(reminded)).toBe(true);
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
    expect(escalationDeadline(m)).toBe(6_000 + ESCALATE_AFTER_MS);
    expect(isAnnouncing(escalateAttention(m))).toBe(true);
  });

  it("keeps an unanswered announcement when the window changes under the player", () => {
    let m = run([reactive("A"), MIN_WAIT_BEFORE_ALERT_MS]);
    m = stepAttention(m, reactive("B"), MIN_WAIT_BEFORE_ALERT_MS + 2_000);
    expect(isAnnouncing(m)).toBe(true);
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

describe("the chime", () => {
  const answeredA = (kind: "reactive" | "planning") =>
    acknowledgeInput(
      run([kind === "reactive" ? reactive("A") : planning("A"), MIN_WAIT_BEFORE_ALERT_MS]),
      5_000,
    );
  const window_ = (kind: "reactive" | "planning", key: string) =>
    kind === "reactive" ? reactive(key) : planning(key);

  it.each(["reactive", "planning"] as const)(
    "is silent for a %s window the player is looking at, however long they were away",
    (kind) => {
      const m = run([window_(kind, "w1"), MIN_WAIT_BEFORE_ALERT_MS * 4]);
      expect(isAnnouncing(m)).toBe(true);
      expect(m.chimeCount).toBe(0);
    },
  );

  it.each(["reactive", "planning"] as const)(
    "sounds once for a %s window that opens while the player is elsewhere, sooner than the away gate",
    (kind) => {
      const m = stepAttention(initialAttentionMachine(0), window_(kind, "w1"), 1_000, "elsewhere");
      expect(isAnnouncing(m)).toBe(true);
      expect(m.chimeCount).toBe(1);
      // Re-commits, and leaving and returning, never sound it again.
      expect(stepAttention(m, window_(kind, "w1"), 2_000, "elsewhere")).toBe(m);
      expect(stepAttention(m, window_(kind, "w1"), 3_000, "attending")).toBe(m);
      expect(noteElsewhere(m, "elsewhere")).toBe(m);
    },
  );

  it("sounds a window held quiet the moment the player leaves, and announces it", () => {
    const quiet = run([planning(), 1_000]);
    expect(isAnnouncing(quiet)).toBe(false);

    const left = noteElsewhere(quiet, "elsewhere");
    expect(isAnnouncing(left)).toBe(true);
    expect(left.chimeCount).toBe(1);
    expect(noteElsewhere(left, "elsewhere")).toBe(left);
  });

  it("sounds an announced window the player was looking at the moment they leave", () => {
    const watched = run([planning(), MIN_WAIT_BEFORE_ALERT_MS]);
    expect(watched.chimeCount).toBe(0);
    expect(noteElsewhere(watched, "attending")).toBe(watched);

    const left = noteElsewhere(watched, "elsewhere");
    expect(left.chimeCount).toBe(1);
    expect(attentionCue(left)).toBe("announced");
  });

  it("changes nothing for an answered window or when no window is held", () => {
    const answered = acknowledgeInput(run([planning(), 1_000]), 2_000);
    expect(noteElsewhere(answered, "elsewhere")).toBe(answered);
    const none = initialAttentionMachine(0);
    expect(noteElsewhere(none, "elsewhere")).toBe(none);
  });

  it("sounds a re-committed window if the player left without a signal reaching the machine", () => {
    const watched = run([planning(), MIN_WAIT_BEFORE_ALERT_MS]);
    const m = stepAttention(watched, planning(), MIN_WAIT_BEFORE_ALERT_MS + 1_000, "elsewhere");
    expect(m.chimeCount).toBe(1);
  });

  it("sounds a reactive window at its reminder when the player never left", () => {
    const m = run([reactive(), MIN_WAIT_BEFORE_ALERT_MS]);
    const reminded = escalateAttention(m);
    expect(reminded.chimeCount).toBe(1);
    expect(isEscalated(reminded)).toBe(true);
  });

  it("sounds a quiet reactive window at its reminder", () => {
    const reminded = escalateAttention(run([reactive(), 1_000]));
    expect(reminded.chimeCount).toBe(1);
  });

  it("does not sound a second time at the reminder after sounding for an absent player", () => {
    const m = stepAttention(initialAttentionMachine(0), reactive(), 1_000, "elsewhere");
    expect(escalateAttention(m).chimeCount).toBe(1);
  });

  it("does not sound a second time for a player who left after the reminder sounded", () => {
    const reminded = escalateAttention(run([reactive(), MIN_WAIT_BEFORE_ALERT_MS]));
    expect(noteElsewhere(reminded, "elsewhere").chimeCount).toBe(1);
  });

  it("never sounds a planning window just for being held", () => {
    let m = run([planning(), MIN_WAIT_BEFORE_ALERT_MS]);
    for (const t of [20_000, 40_000, 120_000]) m = stepAttention(m, planning(), t);
    expect(m.chimeCount).toBe(0);
  });

  it("carries a pending chime to the window that replaces an unanswered one", () => {
    const unanswered = run([planning("A"), MIN_WAIT_BEFORE_ALERT_MS]);
    const next = stepAttention(unanswered, planning("B"), 6_000);
    expect(next.chimeCount).toBe(0);
    expect(noteElsewhere(next, "elsewhere").chimeCount).toBe(1);
  });

  it("does not chime again for a replacement window of a stretch that already sounded", () => {
    const sounded = stepAttention(initialAttentionMachine(0), planning("A"), 1_000, "elsewhere");
    const next = stepAttention(sounded, planning("B"), 2_000, "elsewhere");
    expect(isAnnouncing(next)).toBe(true);
    expect(next.chimeCount).toBe(1);
  });

  it("does not chime again for a replacement window after a reminder sounded", () => {
    const reminded = escalateAttention(run([reactive("A"), MIN_WAIT_BEFORE_ALERT_MS]));
    const next = stepAttention(reminded, reactive("B"), 30_000);
    expect(next.chimeCount).toBe(1);
    expect(escalateAttention(next).chimeCount).toBe(1);
  });

  it.each(["reactive", "planning"] as const)(
    "chimes for a %s replacement window that opens, after an answer, while the player is elsewhere",
    (kind) => {
      const next = stepAttention(answeredA(kind), window_(kind, "B"), 6_000, "elsewhere");
      expect(isAnnouncing(next)).toBe(true);
      expect(next.chimeCount).toBe(1);
    },
  );

  it("leaves an answered window answered and the replacement quiet while attending", () => {
    const a = answeredA("planning");
    expect(stepAttention(a, planning("A"), 6_000, "elsewhere")).toBe(a);
    const quiet = stepAttention(a, planning("B"), 6_000, "attending");
    expect(isAnnouncing(quiet)).toBe(false);
    expect(quiet.chimeCount).toBe(0);
  });

  it("resets an answered stretch: the next unanswered window chimes afresh", () => {
    const first = stepAttention(initialAttentionMachine(0), planning("A"), 1_000, "elsewhere");
    const answered = acknowledgeInput(first, 2_000);
    const next = stepAttention(answered, planning("B"), 3_000, "elsewhere");
    expect(next.chimeCount).toBe(2);
  });

  it("starts a restarted machine from the count it is given", () => {
    const m = initialAttentionMachine(0, 7);
    expect(stepAttention(m, planning(), 1_000, "elsewhere").chimeCount).toBe(8);
  });
});
