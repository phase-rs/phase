/**
 * LLM-driven bot seats in a draft pod.
 *
 * Strictly opt-in and always recoverable: with no configured profile, or on any
 * failure, the pick is submitted through the ordinary path and every bot seat
 * is drafted by the heuristic bot in `draft_wasm::bot_ai`.
 *
 * As in the game path, the engine owns the decision. It renders each seat's own
 * `DraftPlayerView` into a prompt, builds the HTTP request, and resolves the
 * reply into pack cards; this module performs the calls.
 */

import { withDraftEngineOperation } from "../../adapter/draft-adapter";
import type { LlmDraftResponsePayload } from "../../adapter/draft-adapter";
import { ensureSetCatalog } from "../setCatalog";
import { debugLog } from "../../game/debugLog";
import { reportLlmFailure } from "./diagnostics";
import { executeLlmRequest } from "./llmClient";
import { endpointOf, type LlmDraftOutcome, type LlmDraftPickRequest, type LlmProfile } from "./types";

/**
 * Per-seat ceiling for a draft pick.
 *
 * Tighter than the in-game budget because the player is waiting on a click they
 * just made. The call runs with no engine lease held, so a seat that misses the
 * window is simply drafted by the heuristic bot, with no consequence beyond
 * that one card.
 */
export const LLM_DRAFT_TIMEOUT_MS = 20_000;

/**
 * Consecutive failed pick rounds a profile may take before the draft stops
 * calling it for the rest of the session.
 *
 * Mirrors the game-side per-seat breaker in `aiController`. Without one, a
 * provider that is down, rate-limited or simply hanging costs the full request
 * timeout on EVERY pick, turning one misconfiguration into a draft where each
 * click stalls for 20 seconds. The draft keeps going throughout — it just uses
 * the engine bots, which is what it was already falling back to.
 */
const MAX_CONSECUTIVE_DRAFT_FAILURES = 3;

/** Consecutive failed rounds per profile id; cleared by the first success. */
const draftFailures = new Map<string, number>();
/** Profiles given up on for this session. */
const draftDisabled = new Set<string>();

/**
 * The in-flight pick round, so a superseded one can be cut loose.
 *
 * A draft pick is a click the player can repeat, undo or navigate away from
 * while the provider is still thinking. Without this, those calls run to their
 * full timeout, hold sockets, and land replies for a pack that has already
 * passed.
 */
let activeRun: AbortController | null = null;

/**
 * Abandon any in-flight LLM pick round.
 *
 * Safe to call at any time, including when nothing is running. Called when a
 * new round starts and by the draft store when a pick is superseded or the
 * session tears down.
 */
export function cancelLlmDraftRun(): void {
  activeRun?.abort();
  activeRun = null;
}

/** Reset the session breaker. Exposed for tests and session teardown. */
export function resetLlmDraftBreaker(): void {
  draftFailures.clear();
  draftDisabled.clear();
}

/** Whether this profile has been given up on for the session. */
export function isLlmDraftDisabled(profileId: string): boolean {
  return draftDisabled.has(profileId);
}

function recordRound(profileId: string, succeeded: boolean): void {
  if (succeeded) {
    draftFailures.delete(profileId);
    return;
  }
  const failures = (draftFailures.get(profileId) ?? 0) + 1;
  draftFailures.set(profileId, failures);
  if (failures >= MAX_CONSECUTIVE_DRAFT_FAILURES) {
    draftDisabled.add(profileId);
    debugLog(
      `LLM drafter failed ${failures} pick rounds in a row; the engine bots will `
        + "draft for the rest of this session",
      "warn",
    );
  }
}

/** Set code -> printed name, so the format brief reads "Triple Mirrodin". */
async function setNameMap(): Promise<Record<string, string>> {
  const catalog = await ensureSetCatalog();
  if (!catalog) return {};
  return Object.fromEntries(
    Object.entries(catalog).map(([code, info]) => [code.toUpperCase(), info.name]),
  );
}

/**
 * Gather every LLM bot seat's reply for the pick about to be submitted.
 *
 * Deliberately split across the engine-operation lease rather than run inside
 * it. The lease is a SINGLETON serializing every draft engine call, so holding
 * it across a provider round trip would block `getView`, autosave and any other
 * draft operation for the whole request timeout -- up to 20s per pick, on a
 * queue the UI depends on. The three phases here are:
 *
 *   1. build the requests under a short-lived lease (a read-only engine call),
 *   2. perform the network I/O with NO lease held,
 *   3. hand the replies back so the caller can reacquire and submit.
 *
 * Correctness across the gap is the engine's, not this function's: every reply
 * carries the pack fingerprint it was built from, and `submitPickWithLlmBotPicks`
 * re-reads the live pack and refuses any seat whose pack moved on. A slow
 * provider therefore costs that seat its flavour, never a wrong pick.
 *
 * Returns an empty list whenever the LLM path cannot contribute, which the
 * caller treats as "submit the ordinary way".
 */
export async function collectLlmDraftResponses(
  profile: LlmProfile,
  stillCurrent: () => boolean,
): Promise<LlmDraftResponsePayload[]> {
  // A profile the session has given up on skips the round entirely, so a dead
  // provider costs no further latency.
  if (draftDisabled.has(profile.id)) return [];

  // Supersede any round still running for an earlier pick: its replies are for
  // a pack that has moved on, and the engine would refuse them anyway.
  cancelLlmDraftRun();
  const run = new AbortController();
  activeRun = run;

  // Phase 1 -- under lease, read-only. No pick is applied here.
  let requests: LlmDraftPickRequest[];
  try {
    const setNames = await setNameMap();
    // No seat list is supplied. Which seats an LLM may draft for is an
    // AUTHORITY question the draft engine already answers (`DraftSeat::Bot`, in
    // `llm_eligible_bot_seats`), and a second selection here would be a
    // client-side classification of the same thing -- free to drift, and
    // drifting toward disclosing a human seat's private pool.
    requests = await withDraftEngineOperation((lease) =>
      lease.buildLlmDraftPickRequests(JSON.stringify(endpointOf(profile)), setNames),
    );
  } catch (error) {
    // A run the draft lifecycle cancelled (abandon, new draft, resume) will
    // reject here too, and that is not the provider's fault. Charging it would
    // let abandoning three drafts disable a healthy profile.
    if (run.signal.aborted || !stillCurrent()) return [];
    reportLlmFailure("LLM drafters unavailable; using the engine bots", error);
    recordRound(profile.id, false);
    return [];
  }
  if (requests.length === 0) {
    // Nothing to ask is not a provider failure; it is a pod with no eligible
    // seat. It must not count toward giving up on the profile.
    return [];
  }
  if (!stillCurrent() || run.signal.aborted) {
    cancelRun(run);
    return [];
  }

  // Phase 2 -- no lease held. One call per seat, in parallel: the seats pick
  // simultaneously in the rules (CR 905.1a), and serializing them would
  // multiply the pick's latency by the pod size.
  const settled = await Promise.all(
    requests.map(async (request): Promise<LlmDraftResponsePayload | null> => {
      try {
        const { status, body } = await executeLlmRequest(request.request, {
          timeoutMs: LLM_DRAFT_TIMEOUT_MS,
          signal: run.signal,
        });
        return {
          seat: request.seat,
          fingerprint: request.fingerprint,
          provider: profile.provider,
          status,
          body,
        };
      } catch (error) {
        reportLlmFailure(`LLM drafter (seat ${request.seat}) failed`, error);
        return null;
      }
    }),
  );

  const responses = settled.filter((entry): entry is LlmDraftResponsePayload => entry !== null);
  cancelRun(run);

  // A pick superseded mid-flight contributes nothing, even if replies arrived:
  // they describe a pack this seat no longer holds. It is also not the
  // provider's fault, so it is recorded neither way -- charging a cancelled
  // round to the breaker would disable a healthy profile for clicking fast.
  if (!stillCurrent() || run.signal.aborted) return [];

  if (responses.length === 0) {
    // Every call failed at the transport. That is a real failure and the only
    // one this function can judge on its own -- but only once the run is known
    // to still be live, checked immediately above.
    recordRound(profile.id, false);
    return [];
  }

  // Bytes came back, which says nothing about whether they were USABLE: the
  // transport returns HTTP error bodies so a vendor's diagnostic survives, so a
  // round of 401s would otherwise look like a success and reset the breaker.
  // The verdict belongs to the engine and arrives at submit time, via
  // `recordLlmDraftSubmission`.
  return responses;
}

/**
 * Record a round's outcome from the ENGINE's verdict on each seat's reply.
 *
 * Called by the draft store once `submitPickWithLlmBotPicks` has ruled. A round
 * counts as a success only when the engine actually used at least one seat's
 * pick; a round where every reply was refused -- an error body, an undecodable
 * completion, a pack that moved on -- counts against the profile, which is what
 * lets the breaker trip on a provider that is answering but useless.
 */
export function recordLlmDraftSubmission(profileId: string, outcomes: LlmDraftOutcome[]): void {
  recordRound(
    profileId,
    outcomes.some((outcome) => outcome.used),
  );
}

/** Clear `activeRun` only if it still refers to this round. */
function cancelRun(run: AbortController): void {
  if (activeRun === run) activeRun = null;
}

/**
 * Report what the engine did with each seat's reply.
 *
 * Only failures are reported. A drafter's reasoning is derived from its own
 * pack and pool -- both private to that seat -- and `debugLog` writes a
 * `visibility: "Public"` entry into the shared game log, so publishing it would
 * turn hidden draft information into a public artifact.
 */
export function reportLlmDraftOutcomes(outcomes: LlmDraftOutcome[]): void {
  for (const outcome of outcomes) {
    if (!outcome.used && outcome.error) {
      // `outcome.error` can carry a provider-authored diagnostic, and the game
      // log is prompt-renderable, so only the seat is named there.
      reportLlmFailure(
        `LLM drafter (seat ${outcome.seat}) fell back to the engine bot`,
        outcome.error,
      );
    }
  }
}

