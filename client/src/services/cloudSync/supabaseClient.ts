import {
  createClient,
  type RealtimeChannel,
  type SupabaseClient,
} from "@supabase/supabase-js";

/**
 * Build-time config injected via Vite defines (see vite.config.ts). The anon
 * key is PUBLIC by design — Row-Level Security is the actual access control, so
 * shipping it in the client bundle is its intended use, not a leak. Both are
 * empty when a deployment doesn't configure Supabase (e.g. self-hosters), which
 * disables cloud sync and leaves file backup as the only data-portability path.
 *
 * The `typeof` guard keeps this module importable under Vitest, where the
 * defines may be absent.
 */
const SUPABASE_URL =
  typeof __SUPABASE_URL__ !== "undefined" ? __SUPABASE_URL__ : "";
const SUPABASE_ANON_KEY =
  typeof __SUPABASE_ANON_KEY__ !== "undefined" ? __SUPABASE_ANON_KEY__ : "";

export function isSupabaseConfigured(): boolean {
  return SUPABASE_URL.length > 0 && SUPABASE_ANON_KEY.length > 0;
}

let client: SupabaseClient | null = null;
type DesiredTransportState = "paused" | "resumed";
type AppliedTransportState =
  | DesiredTransportState
  | "cleanup-pending"
  | "channels-pending";

let desiredTransportState: DesiredTransportState = "paused";
let appliedTransportState: AppliedTransportState = "paused";
interface TransportTransition {
  readonly promise: Promise<void>;
  resolve(): void;
  reject(reason: unknown): void;
}

let activeTransportTransition: TransportTransition | null = null;

/**
 * Lazily construct the singleton client. Callers must guard with
 * `isSupabaseConfigured()` first — calling this when unconfigured throws.
 */
export function getSupabaseClient(): SupabaseClient {
  if (!client) {
    client = createClient(SUPABASE_URL, SUPABASE_ANON_KEY, {
      auth: {
        // Lifecycle callers explicitly start this after online resume so a
        // cold offline boot creates no background refresh work.
        persistSession: true,
        autoRefreshToken: false,
        // Process the OAuth fragment on return from the provider redirect.
        detectSessionInUrl: true,
      },
    });
  }
  if (
    desiredTransportState === "paused" &&
    appliedTransportState === "paused"
  ) {
    // Even though construction disables auto-refresh, a caller can use the
    // client to create realtime state while offline. The next pause owns the
    // authoritative SDK cleanup instead of assuming this remains inert.
    appliedTransportState = "cleanup-pending";
  }
  return client;
}

/** Starts the Supabase SDK's own background token refresh lifecycle. */
export function resumeSupabaseClient(): Promise<void> {
  return requestTransportState("resumed");
}

/** Stops SDK refresh and lets Supabase remove every realtime channel. */
export function pauseSupabaseClient(): Promise<void> {
  return requestTransportState("paused");
}

/**
 * Retries public SDK channel removal after disconnecting the transport, then
 * confirms the Realtime registry no longer owns any of the channels.
 */
export async function recoverSupabaseChannelRemoval(
  supabase: SupabaseClient,
  channels: readonly RealtimeChannel[],
): Promise<void> {
  const recoveryIssues: string[] = [];
  try {
    const status = await supabase.realtime.disconnect();
    if (status !== "ok") recoveryIssues.push(`disconnect returned ${status}`);
  } catch (error) {
    recoveryIssues.push(`disconnect rejected: ${String(error)}`);
  }
  for (const channel of channels) {
    try {
      const status = await supabase.removeChannel(channel);
      if (status !== "ok") {
        recoveryIssues.push(`removeChannel returned ${status}`);
      }
    } catch (error) {
      recoveryIssues.push(`removeChannel rejected: ${String(error)}`);
    }
  }
  let registeredChannels: readonly RealtimeChannel[];
  try {
    registeredChannels = supabase.realtime.getChannels();
  } catch (error) {
    throw new Error(
      `Could not verify Supabase realtime channel removal: ${String(error)}`,
    );
  }
  const remainingChannels = channels.filter((channel) =>
    registeredChannels.includes(channel),
  );
  if (remainingChannels.length > 0) {
    const issueContext = recoveryIssues.length > 0
      ? ` (${recoveryIssues.join("; ")})`
      : "";
    throw new Error(
      `Supabase realtime channels remain registered after recovery: ${remainingChannels.length}/${channels.length}${issueContext}`,
    );
  }
}

function requestTransportState(state: DesiredTransportState): Promise<void> {
  desiredTransportState = state;
  const activeTransition = activeTransportTransition;
  if (!activeTransition) {
    const transition = createTransportTransition();
    activeTransportTransition = transition;
    void drainTransportState(transition);
    return transition.promise;
  }
  return activeTransition.promise;
}

function createTransportTransition(): TransportTransition {
  let resolve!: () => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<void>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

async function drainTransportState(transition: TransportTransition): Promise<void> {
  try {
    while (appliedTransportState !== desiredTransportState) {
      if (desiredTransportState === "resumed") {
        try {
          await getSupabaseClient().auth.startAutoRefresh();
        } catch (error) {
          // A failed start may have allocated SDK transport state. Preserve
          // cleanup ownership if a pause superseded it; otherwise reject so a
          // later resume can retry the start.
          appliedTransportState = "cleanup-pending";
          if (desiredTransportState !== "resumed") continue;
          throw error;
        }
        appliedTransportState = "resumed";
      } else if (
        appliedTransportState === "resumed" ||
        appliedTransportState === "cleanup-pending"
      ) {
        try {
          await client!.auth.stopAutoRefresh();
        } catch (error) {
          if (desiredTransportState !== "paused") {
            // Stop may have succeeded despite rejecting. Re-establish refresh
            // rather than assuming the earlier resumed state still holds.
            appliedTransportState = "paused";
            continue;
          }
          // The caller still needs a paused transport, so retain the
          // conservative running state and leave the next pause retryable.
          appliedTransportState = "resumed";
          throw error;
        }
        // Refresh is already stopped if removing channels fails. Retain this
        // truthful partial state so resume restarts refresh and pause retries
        // only the cleanup that is still outstanding.
        appliedTransportState = "channels-pending";
      }

      if (
        desiredTransportState === "paused" &&
        appliedTransportState === "channels-pending"
      ) {
        try {
          await client!.removeAllChannels();
          const registeredChannels = [...client!.realtime.getChannels()];
          if (registeredChannels.length > 0) {
            await recoverSupabaseChannelRemoval(client!, registeredChannels);
          }
          if (client!.realtime.getChannels().length > 0) {
            throw new Error("Supabase realtime channels remain registered after removal");
          }
        } catch (error) {
          // Refresh is known stopped, but channel cleanup is unresolved. If a
          // resume superseded this pause, the next loop establishes refresh.
          if (desiredTransportState !== "paused") continue;
          throw error;
        }
        appliedTransportState = "paused";
      }
    }

    // Clear the active transition before waking callers. A request arriving
    // after the final equality check therefore starts a new drain instead of
    // attaching to a promise whose cleanup is already pending.
    if (activeTransportTransition === transition) {
      activeTransportTransition = null;
      transition.resolve();
    }
  } catch (error) {
    if (activeTransportTransition === transition) {
      activeTransportTransition = null;
      transition.reject(error);
    }
  }
}
