/**
 * Runtime-local ownership of Rust state-codec decoder baselines.
 *
 * The wire carries negotiated StateCodecV1 Full/Delta frames. Decoding yields
 * the exact `ServerMessage` JSON the snapshot transport would have sent.
 * Rust owns reconstruction and integrity: TypeScript never
 * diffs, patches, hashes or inspects the decoded state. This module only keeps
 * the retained baseline handle for one decoder session and hands frames to the
 * consuming WASM API.
 *
 * One instance lives per engine runtime: the engine worker owns the instances
 * for its WASM module, and the shared main-thread runtime owns the fallback's.
 * Session ids are monotonically allocated within a runtime and are never game
 * ids or revisions. Only `open` creates an entry; decoding on a closed or
 * unknown id is a lifecycle failure, never an implicit reopen.
 */
import type { EngineModule } from "./engine-queries";
import type { StateCodecHandle } from "@wasm/engine";

/** Decode RPC / fallback caller deadline. A timeout is fatal to the socket. */
export const STATE_CODEC_DECODE_TIMEOUT_MS = 60_000;

/**
 * A lifecycle failure (unknown session, unreadable decoded text, disposed
 * runtime). Distinct from a codec integrity rejection, which is reported as
 * `resyncRequired`.
 */
export class StateCodecLifecycleError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "StateCodecLifecycleError";
  }
}

export type StateCodecDecodeResult =
  | { readonly kind: "decoded"; readonly json: string }
  | { readonly kind: "resyncRequired" };

export class StateCodecRuntime {
  /** Retained baseline per open session; `null` until a Full establishes one. */
  private readonly baselines = new Map<number, StateCodecHandle | null>();
  private nextSessionId = 1;
  private disposed = false;

  constructor(private readonly engine: EngineModule) {}

  /** Allocate an empty decoder session (no baseline yet). */
  open(): number {
    if (this.disposed) throw new StateCodecLifecycleError("State-codec runtime is disposed");
    const sessionId = this.nextSessionId++;
    this.baselines.set(sessionId, null);
    return sessionId;
  }

  /**
   * Decode one frame against the session's retained baseline.
   *
   * The retained baseline is cloned before transfer: the WASM API consumes the
   * handle it is given, on success and on rejection alike. A successful decode
   * replaces the retained baseline with the candidate (the previous handle is
   * freed after its text is read). A rejected frame drops the retained
   * baseline and reports `resyncRequired`, because the client can no longer
   * prove it shares the server's delivered-frame baseline.
   */
  decode(sessionId: number, frame: Uint8Array): StateCodecDecodeResult {
    if (!this.baselines.has(sessionId)) {
      throw new StateCodecLifecycleError(`Unknown state-codec session ${sessionId}`);
    }
    const retained = this.baselines.get(sessionId) ?? null;
    let transfer: StateCodecHandle | null = null;
    if (retained !== null) {
      try {
        transfer = retained.clone_handle();
      } catch {
        // The retained handle is unusable; drop it so the session can only be
        // recovered by a Full, never by a delta against a stale baseline.
        this.dropBaseline(sessionId);
        throw new StateCodecLifecycleError("State-codec baseline could not be cloned");
      }
    }
    let candidate: StateCodecHandle;
    try {
      candidate = this.engine.state_codec_decode(frame, transfer ?? undefined);
    } catch (error) {
      this.dropBaseline(sessionId);
      // A wasm trap (panic, `unreachable`) is not a codec verdict: the instance
      // may be left inconsistent, so it is a lifecycle failure the socket
      // terminates on instead of resyncing against the same instance.
      if (error instanceof WebAssembly.RuntimeError) {
        throw new StateCodecLifecycleError("State-codec decoder trapped");
      }
      // Rust rejected the frame: malformed frame, missing baseline, base hash
      // mismatch, or result hash mismatch. Integrity is the codec crate's
      // verdict, so this is a resync request, not a lifecycle failure.
      return { kind: "resyncRequired" };
    }
    let json: string;
    try {
      json = candidate.json();
    } catch {
      candidate.free();
      this.dropBaseline(sessionId);
      throw new StateCodecLifecycleError("Decoded state text could not be read");
    }
    this.baselines.set(sessionId, candidate);
    retained?.free();
    return { kind: "decoded", json };
  }

  /** Release one session and its retained baseline. Unknown ids are ignored. */
  close(sessionId: number): void {
    this.dropBaseline(sessionId);
    this.baselines.delete(sessionId);
  }

  dispose(): void {
    this.disposed = true;
    for (const sessionId of [...this.baselines.keys()]) this.close(sessionId);
  }

  private dropBaseline(sessionId: number): void {
    if (!this.baselines.has(sessionId)) return;
    const retained = this.baselines.get(sessionId) ?? null;
    this.baselines.set(sessionId, null);
    retained?.free();
  }
}

/** The decoder surface one prepared transport session exposes to its socket. */
export interface StateCodecSession {
  decode(frame: Uint8Array): Promise<StateCodecDecodeResult>;
  close(): void;
}
