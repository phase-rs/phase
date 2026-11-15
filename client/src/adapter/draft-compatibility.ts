/**
 * Draft27 understands only the legacy player-view contract.  This classifier
 * intentionally examines the raw normalized record rather than the legacy
 * TypeScript DTO, which cannot describe later protocol states safely.
 */
export type Draft27CompatibilityError = {
  kind: "incompatible";
  code: "fifo_flow" | "review_status" | "residual_fifo" | "engine_refusal";
  message: string;
};

export type Draft27ViewAdmission =
  | { ok: true }
  | { ok: false; error: Draft27CompatibilityError };

/**
 * The host must retain the exact value it admitted.  Keeping the generic
 * value in this result makes it impossible for a caller to classify one view
 * and accidentally publish a later reread instead.
 */
export type Draft27ValueAdmission<T> =
  | { ok: true; value: T }
  | { ok: false; error: Draft27CompatibilityError };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * Admits only draft27's legacy view shape without changing the input or
 * producing any delivery-side effect.  The order is part of the protocol
 * boundary: a known FIFO flow is more specific than its status or payload.
 */
export function classifyDraft27Compatibility(rawView: unknown): Draft27ViewAdmission {
  if (!isRecord(rawView)) return { ok: true };

  if (rawView.flow === "FifoAsync") {
    return {
      ok: false,
      error: {
        kind: "incompatible",
        code: "fifo_flow",
        message: "This draft uses FIFO delivery, which this connection cannot accept.",
      },
    };
  }

  if (rawView.status === "Review") {
    return {
      ok: false,
      error: {
        kind: "incompatible",
        code: "review_status",
        message: "This draft is in Review, which this connection cannot accept.",
      },
    };
  }

  if (Object.prototype.hasOwnProperty.call(rawView, "fifo") && rawView.fifo !== null) {
    return {
      ok: false,
      error: {
        kind: "incompatible",
        code: "residual_fifo",
        message: "This draft includes FIFO state, which this connection cannot accept.",
      },
    };
  }

  return { ok: true };
}

/**
 * Host and import callers use the generic entry below so their admitted raw
 * value remains bound to the side effects that follow it.
 */
export function admitDraft27GuestView(rawView: unknown): Draft27ViewAdmission {
  return classifyDraft27Compatibility(rawView);
}

/**
 * Side-effect-free host admission for a raw view.  It classifies only the
 * value it is given: a saved draft session is the draft engine's to check.
 */
export function admitDraft27HostValue<T>(value: T): Draft27ValueAdmission<T> {
  const direct = classifyDraft27Compatibility(value);
  if (!direct.ok) return direct;

  return { ok: true, value };
}

/** Returns the admitted value, or throws the typed incompatibility error. */
export function requireDraft27HostValue<T>(value: T): T {
  const admission = admitDraft27HostValue(value);
  if (!admission.ok) throw new Draft27IncompatibilityError(admission.error);
  return admission.value;
}

export class Draft27IncompatibilityError extends Error {
  readonly kind = "incompatible" as const;
  readonly code: Draft27CompatibilityError["code"];

  constructor({ code, message }: Draft27CompatibilityError) {
    super(message);
    this.name = "Draft27IncompatibilityError";
    this.code = code;
  }
}

/**
 * The draft engine's refusal of a saved session it will not restore. It
 * classifies no value: the engine reports only that it refused the session.
 */
export function draftEngineRefusal(): Draft27IncompatibilityError {
  return new Draft27IncompatibilityError({
    kind: "incompatible",
    code: "engine_refusal",
    message: "The draft engine cannot restore this saved draft.",
  });
}
