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

import type {
  DraftEngineOperationLease,
  DraftPlayerView,
  LlmDraftResponsePayload,
} from "../../adapter/draft-adapter";
import { ensureSetCatalog } from "../setCatalog";
import { debugLog } from "../../game/debugLog";
import { executeLlmRequest } from "./llmClient";
import { endpointOf, type LlmProfile } from "./types";

/**
 * Per-seat ceiling for a draft pick.
 *
 * Tighter than the in-game budget: a pick blocks the draft's engine-operation
 * queue while it runs, and a seat that misses the window is drafted by the
 * heuristic bot with no visible consequence beyond that one card.
 */
export const LLM_DRAFT_TIMEOUT_MS = 20_000;

/** Set code -> printed name, so the format brief reads "Triple Mirrodin". */
async function setNameMap(): Promise<Record<string, string>> {
  const catalog = await ensureSetCatalog();
  if (!catalog) return {};
  return Object.fromEntries(
    Object.entries(catalog).map(([code, info]) => [code.toUpperCase(), info.name]),
  );
}

/**
 * Submit the human's pick with LLM-driven bot seats.
 *
 * Returns the authoritative post-pick view. Falls back to `lease.submitPick`
 * whenever the LLM path cannot run at all; individual seat failures are handled
 * inside the engine and reported through `llmOutcomes`.
 */
export async function submitPickWithLlmBots(
  lease: DraftEngineOperationLease,
  cardInstanceId: string,
  profile: LlmProfile,
  botSeats: number[],
): Promise<DraftPlayerView> {
  if (botSeats.length === 0) return lease.submitPick(cardInstanceId);

  let requests;
  try {
    requests = lease.buildLlmDraftPickRequests(
      JSON.stringify(endpointOf(profile)),
      botSeats,
      await setNameMap(),
    );
  } catch (error) {
    debugLog(`LLM drafters unavailable; using the engine bots: ${describe(error)}`, "warn");
    return lease.submitPick(cardInstanceId);
  }
  if (requests.length === 0) return lease.submitPick(cardInstanceId);

  // One call per seat, in parallel: the seats pick simultaneously in the rules
  // (CR 905.1a), and serializing them would multiply the pick's latency by the
  // pod size.
  const settled = await Promise.all(
    requests.map(async (request): Promise<LlmDraftResponsePayload | null> => {
      try {
        const body = await executeLlmRequest(request.request, {
          timeoutMs: LLM_DRAFT_TIMEOUT_MS,
        });
        return {
          seat: request.seat,
          fingerprint: request.fingerprint,
          provider: profile.provider,
          body,
        };
      } catch (error) {
        debugLog(`LLM drafter (seat ${request.seat}) failed: ${describe(error)}`, "warn");
        return null;
      }
    }),
  );

  const responses = settled.filter((entry): entry is LlmDraftResponsePayload => entry !== null);
  if (responses.length === 0) return lease.submitPick(cardInstanceId);

  const { view: nextView, llmOutcomes } = lease.submitPickWithLlmBotPicks(
    cardInstanceId,
    responses,
  );
  for (const outcome of llmOutcomes) {
    if (outcome.used) {
      if (outcome.reasoning) {
        debugLog(`LLM drafter (seat ${outcome.seat}): ${outcome.reasoning}`, "info");
      }
    } else if (outcome.error) {
      debugLog(
        `LLM drafter (seat ${outcome.seat}) fell back to the engine bot: ${outcome.error}`,
        "warn",
      );
    }
  }
  return nextView;
}

/** Bot seats in a pod, in seat order. Seat 0 is the local player. */
export function botSeatIndices(view: DraftPlayerView): number[] {
  return view.seats.filter((seat) => seat.is_bot).map((seat) => seat.seat_index);
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
