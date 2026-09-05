import type { TFunction } from "i18next";
import { type ReactNode, useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type {
  AmountAssignment,
  InteractionChoice,
  InteractionChoiceId,
  InteractionId,
  InteractionPresentationSurface,
  InteractionPreview,
  InteractionPreviewRequest,
  InteractionResponse,
  InteractionResponseSpec,
  InteractionShortcutDecision,
  InteractionShortcutPin,
  InteractionShortcutPoint,
  InteractionShortcutPreview,
  InteractionSubmission,
  PreviewRequestId,
  ViewerInteraction,
} from "../../adapter/generated/interaction";
import type { IterationCount, ResourceAxis, WaitingFor, WinKind } from "../../adapter/types.ts";
import { dispatchInteraction, previewInteractionResponse } from "../../game/dispatch.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { familyOf, UNBOUNDED_FAMILY_LABEL_KEY, UnboundedBadge } from "../hud/HudBadges.tsx";
import { AmountInput, parseAmount } from "../mana/AmountInput.tsx";
import { DialogShell } from "./DialogShell.tsx";

/**
 * CR 732.2a/b/c: the interactive loop-shortcut declare + accept-or-shorten
 * modals. A game MAGNITUDE — a consequence the shortcut causes, such as life lost or cards
 * milled — is rendered only as a direct read of a published engine schema/proposal/response-spec
 * field; the client never computes, scales or re-attributes one. The player's own DECLARATION is
 * the other side of that line: the count and its partition across published `choice_id`s are
 * authored, summed and displayed here for form state and button enablement, and the engine
 * validates it independently. `DeclareShortcut.template` is always `null`: constructing a
 * `DecisionTemplate` is not a client authority, and the engine remains the sole legality authority
 * (`predictability_gate` + `validate_pins`).
 *
 * MEASURED LIMIT, stated rather than assumed — `null` is what the client can honestly send,
 * NOT a payload the engine accepts everywhere. `handle_declare_shortcut`
 * (`game/engine.rs`, the `!offer.schema.points.is_empty()` block) REFUSES a `template: null`
 * declaration unless the proposer controls the recorded loop period. Carrying the engine's own
 * issued declaration through the manual declare path is an ENGINE-side repair; a client that
 * reconstructed a template would be inventing rules authority it does not have.
 */

/** The engine's published shortcut response spec — every field this modal reads is a lookup
 *  into what the engine already sent, never a derivation. */
type ShortcutSpec = Extract<InteractionResponseSpec, { type: "shortcut" }>["data"];

function shortcutSpec(interaction: ViewerInteraction | null): ShortcutSpec | null {
  for (const opportunity of interaction?.opportunities ?? []) {
    if (opportunity.response.type !== "schema") continue;
    const { spec } = opportunity.response.data;
    if (spec.type === "shortcut") return spec.data;
  }
  return null;
}

/** The live shortcut offer's interaction id — the identity React keys the offer body on.
 *  Walks the same list under the same predicate as `shortcutSpec`, so the two agree on which
 *  opportunity is "the offer" by construction rather than by convention.
 *
 *  Returns the branded string ITSELF, never a wrapper object. `useGameStore` is zustand 5, whose
 *  `useStore` is a bare `useSyncExternalStore` with no equality function, so React compares
 *  successive selector results with `Object.is`: a selector returning a fresh object literal is
 *  the documented infinite-loop shape, not merely an extra render. `InteractionId` is
 *  `string & { __brand }` — a primitive at runtime — so this is `Object.is`-stable whenever the
 *  id has not rotated, for the same reason `shortcutSpec`'s store reference is. */
function shortcutInteractionId(interaction: ViewerInteraction | null): InteractionId | null {
  for (const opportunity of interaction?.opportunities ?? []) {
    if (opportunity.response.type !== "schema") continue;
    if (opportunity.response.data.spec.type === "shortcut") return opportunity.interactionId;
  }
  return null;
}

/** The live offer's published candidates — the choices its decision points name by id. Walks the
 *  same list under the same predicate as the two selectors above.
 *
 *  Returns a reference INTO store state, or `null`; never `[]`. See `shortcutInteractionId`'s
 *  stability note — a selector minting a fresh array literal is the same `Object.is` shape. The
 *  caller supplies the empty default outside the selector. */
function shortcutCandidates(interaction: ViewerInteraction | null): InteractionChoice[] | null {
  for (const opportunity of interaction?.opportunities ?? []) {
    if (opportunity.response.type !== "schema") continue;
    if (opportunity.response.data.spec.type === "shortcut") {
      return opportunity.response.data.candidates;
    }
  }
  return null;
}

/** The engine's published accept-or-shorten response spec, carrying the declaration this player
 *  is being asked to judge. Every field the responder modal reads is a lookup into what the
 *  engine already sent, never a derivation. */
type ShortcutReplySpec = Extract<InteractionResponseSpec, { type: "shortcutReply" }>["data"];

/** Line-for-line sibling of `shortcutSpec`, one predicate apart. Returns a reference INTO store
 *  state, or `null`; see `shortcutInteractionId`'s `Object.is` stability note. */
function shortcutReplySpec(interaction: ViewerInteraction | null): ShortcutReplySpec | null {
  for (const opportunity of interaction?.opportunities ?? []) {
    if (opportunity.response.type !== "schema") continue;
    const { spec } = opportunity.response.data;
    if (spec.type === "shortcutReply") return spec.data;
  }
  return null;
}

/** The declaration's published candidates — the subjects and answers its statement points name
 *  by id. Walks the same list under the same predicate as the selector above.
 *
 *  Returns a reference INTO store state, or `null`; never `[]`. The caller supplies the empty
 *  default outside the selector. */
function shortcutReplyCandidates(
  interaction: ViewerInteraction | null,
): InteractionChoice[] | null {
  for (const opportunity of interaction?.opportunities ?? []) {
    if (opportunity.response.type !== "schema") continue;
    if (opportunity.response.data.spec.type === "shortcutReply") {
      return opportunity.response.data.candidates;
    }
  }
  return null;
}

/** Which control the offer's announced-target point opens, CARRYING the point it opens on, so
 *  routing, rendering and the dispatch cannot disagree about which point is answered. */
type TargetsControl = { kind: "allocation" | "subject"; point: InteractionShortcutPoint };

/** Positional equality of two allocations. Not `JSON.stringify`: stringify equality also depends
 *  on key insertion order, which would move the gate for a reason unrelated to the allocation. */
function sameAllocation(a: AmountAssignment[], b: AmountAssignment[]): boolean {
  return (
    a.length === b.length &&
    a.every((x, i) => x.choiceId === b[i].choiceId && x.amount === b[i].amount)
  );
}

/**
 * CR 601.2c: the announcement subject a candidate names, read off the engine's own surfaces —
 * a player seat or an object. The seat label is frontend chrome and is translated; an object's
 * `name` is card/engine pass-through and is rendered RAW, falling back to the published
 * `reference` because the binding declares `name` nullable (`client/src/i18n/README.md`).
 */
function candidateLabel(
  t: TFunction<"game">,
  candidates: InteractionChoice[],
  id: InteractionChoiceId,
): string {
  const surfaces = candidates.find((c) => c.id === id)?.surfaces ?? [];
  const player = surfaces.find(
    (s): s is Extract<InteractionPresentationSurface, { type: "player" }> => s.type === "player",
  );
  // The same +1 display formatting `PreviewLines` applies to `entry.player`. Formatting, not
  // derivation.
  if (player) return t("lifeTotal.playerLabel", { seat: player.data.seat + 1 });
  const object = surfaces.find(
    (s): s is Extract<InteractionPresentationSurface, { type: "object" }> => s.type === "object",
  );
  if (object) return object.data.name ?? object.data.reference;
  return id;
}

/** A `mayChoice` candidate's published discriminant (`take` / `decline`). The enum string itself
 *  is never rendered — the button copy is frontend-authored chrome keyed off this value. */
function mayCandidate(candidates: InteractionChoice[], id: InteractionChoiceId): string | null {
  const surfaces = candidates.find((c) => c.id === id)?.surfaces ?? [];
  const value = surfaces.find(
    (s): s is Extract<InteractionPresentationSurface, { type: "value" }> => s.type === "value",
  );
  return value?.data.value ?? null;
}

/**
 * The one shape every per-subject control in this modal is built from. A control that asks the
 * player about a specific subject — a named victim, one particular optional ability — states that
 * subject visibly beside itself, alongside whatever accessible name the control itself carries.
 * A subject reachable only through an `aria-label` leaves a sighted player looking at N identical
 * controls; a subject reachable only through visible text is unreachable by a screen reader. Both
 * are required: this renders the text, and the control it wraps carries the accessible name.
 */
function SubjectControl({ subject, children }: { subject: string; children: ReactNode }) {
  return (
    <div className="flex flex-wrap items-center gap-2">
      <span className="grow text-sm text-slate-200">{subject}</span>
      {children}
    </div>
  );
}

// CR 732.1b: render the engine-proposed repeat mode — the offer's own stated count, echoed
// verbatim. The picker below narrows WITHIN the engine's published window; this line is the
// offer, not the pick.
function CountLine({ count }: { count: IterationCount }) {
  const { t } = useTranslation("game");
  return (
    <p className="text-sm text-slate-300">
      {count === "UntilLethal"
        ? t("comboShortcut.untilLethal")
        : t("comboShortcut.fixedCount", { count: count.Fixed })}
    </p>
  );
}

// CR 704.5a/704.5c etc.: the certificate's determinate win kind, a pure key lookup.
function WinKindLine({ kind }: { kind: WinKind }) {
  const { t } = useTranslation("game");
  return <p className="text-sm font-semibold text-white">{t(`comboShortcut.winKind.${kind}`)}</p>;
}

// Reuses the engine-authored HUD family mapping (`familyOf`) + badge — no new
// axis logic, no new i18n keys. Dedupes by display family like the HUD caller.
function FamilyBadges({ axes }: { axes: ResourceAxis[] }) {
  const families = [...new Set(axes.map(familyOf))];
  if (families.length === 0) return null;
  return (
    <div className="flex flex-wrap gap-1">
      {families.map((family) => (
        <UnboundedBadge key={family} family={family} />
      ))}
    </div>
  );
}

/**
 * CR 732.2a: what the offer's stated count actually DOES, per axis. Every magnitude is read
 * straight off the engine's published preview — already multiplied, already signed. The heading
 * names `preview.count` because these numbers describe that count and no other, so a player can
 * never read them against a different one; the display layer multiplies nothing.
 */
function PreviewLines({ preview }: { preview: InteractionShortcutPreview }) {
  const { t } = useTranslation("game");
  if (preview.entries.length === 0) return null;
  return (
    <div className="flex flex-col gap-1 rounded-lg bg-white/5 px-3 py-2">
      <p className="text-xs font-semibold tracking-wide text-slate-400 uppercase">
        {t("comboShortcut.previewTitle", { count: preview.count })}
      </p>
      {preview.entries.map((entry, index) => (
        <p
          key={`${entry.family}-${entry.player ?? "all"}-${index}`}
          className="text-sm text-slate-200 tabular-nums"
        >
          {entry.player === null
            ? t("comboShortcut.previewEntry", {
                amount: entry.amount,
                resource: t(UNBOUNDED_FAMILY_LABEL_KEY[entry.family]),
              })
            : t("comboShortcut.previewEntryPlayer", {
                amount: entry.amount,
                resource: t(UNBOUNDED_FAMILY_LABEL_KEY[entry.family]),
                // Seat display numbering, the same +1 formatting `LifeTotal` uses on the engine's
                // seat id. Formatting, not derivation.
                player: t("lifeTotal.playerLabel", { seat: entry.player + 1 }),
              })}
        </p>
      ))}
    </div>
  );
}

/**
 * CR 732.2a: the priority holder (the proposer) may declare the shortcut OR decline it —
 * "the player with priority may suggest a shortcut" is
 * optional. Declining dispatches `DeclineShortcut`, which restores ordinary
 * priority engine-side; the opponent-side escape hatch (accept/shorten) lives in
 * `RespondToShortcutModal`.
 */
export function DeclareShortcutModal() {
  const canAct = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  // `shortcutSpec` returns a reference INTO store state (or null), so the selector is stable.
  const spec = useGameStore((s) => shortcutSpec(s.viewerInteraction));
  // A branded string, not an object literal — same snapshot-stability reason as the line above.
  const offerId = useGameStore((s) => shortcutInteractionId(s.viewerInteraction));

  if (waitingFor?.type !== "LoopShortcut" || !canAct) return null;

  // A typed count must not survive into a LATER offer, and the two ways one offer can follow
  // another need two different mechanisms. Naming only the first is how this comment was wrong:
  //   - offer -> other-state -> offer: covered by the `return null` above, which unmounts the body
  //     when the state leaves `LoopShortcut`. This component itself never unmounts (GamePage keeps
  //     both modals mounted and they self-gate), so that guard is the only unmount there is.
  //   - offer -> offer: covered by the `key` below and ONLY by it. React reconciles by element type
  //     and position, so without a key two consecutive offers share one `DeclareShortcutOffer`
  //     instance and its `picked`. The transport may deliver B without the client ever committing a
  //     render at an intermediate state, so the first guard is not merely weaker here — it never runs.
  // `interactionId` is the identity because it rotates in ENGINE state on every accepted action:
  // `LoopShortcut` classifies as a non-simultaneous Single decision, and
  // `rebind_interaction_slots_after_action` re-mints those "including A→A and A→B→A". A key built
  // from the published window, or from any `waitingFor.data` field, is not distinct between two
  // offers that happen to carry equal values — the plausible fix that reads as a fix.
  // `offerId` is null only when no shortcut opportunity is published, and then `spec` is null too
  // (one predicate, one list), so no picker renders and there is no `picked` to leak.
  return <DeclareShortcutOffer key={offerId ?? "no-offer"} data={waitingFor.data} spec={spec} />;
}

function DeclareShortcutOffer({
  data,
  spec,
}: {
  data: Extract<WaitingFor, { type: "LoopShortcut" }>["data"];
  spec: ShortcutSpec | null;
}) {
  const { t } = useTranslation("game");
  const dispatch = useGameStore((s) => s.dispatch);
  // Read here rather than passed down: the routing rule and the submission's `interactionId` both
  // need it, and the parent already reads it for the `key`.
  const offerId = useGameStore((s) => shortcutInteractionId(s.viewerInteraction));
  const publishedCandidates = useGameStore((s) => shortcutCandidates(s.viewerInteraction));
  const candidates = publishedCandidates ?? [];
  const { certificate, schema } = data;

  // CR 732.2a: the offer's own published points and preview decide which declaration the client
  // may author. Null-safe at this component's own typing — `spec` is the prop's declared
  // `ShortcutSpec | null`.
  const points = spec?.points ?? [];
  const previewElements = spec?.preview ?? [];
  // CR 601.2c: the announced-target point the allocation is stated over.
  const allocationPoint = points.find((p) => p.kind === "targets") ?? null;
  // Computed AFTER `allocationPoint` and null whenever it is absent, so no arm can name a control
  // with no candidate list to stand on.
  const targetsControl: TargetsControl | null =
    allocationPoint === null
      ? null
      : spec?.count.type === "untilLethal"
        ? { kind: "subject", point: allocationPoint }
        : previewElements.length > 0
          ? { kind: "allocation", point: allocationPoint }
          : null;

  const renderable = (p: InteractionShortcutPoint): boolean => {
    switch (p.kind) {
      case "mayChoice":
        // Its domain is its OWN candidate list, never `targetsControl`: a may point must be
        // answerable whether or not the offer also publishes a `targets` point.
        return p.candidateIds.length > 0;
      case "targets":
        return (
          targetsControl !== null &&
          p.group === targetsControl.point.group &&
          p.max === 1 &&
          p.candidateIds.length > 0
        );
      default:
        // A whitelist, deliberately: `InteractionShortcutPointKind` is a plain string union
        // with no compile-time exhaustiveness at a `!==` test, so a kind this modal has no
        // control for must default to NOT renderable.
        return false;
    }
  };

  const pinRoute =
    offerId !== null &&
    spec !== null &&
    points.length > 0 &&
    points.some((p) => !p.readOnly) &&
    points.every((p) => p.readOnly || renderable(p));

  // The may points this offer lets the player answer. Empty off the route BY CONSTRUCTION, so the
  // route gate lives in the binding and cannot be omitted at one of its consumers.
  const mayPoints = pinRoute ? points.filter((p) => !p.readOnly && p.kind === "mayChoice") : [];

  // CR 732.2a: the count window is ENGINE-OWNED. `null` when this offer publishes no finite
  // window (UntilLethal) or when the transport published no interaction projection at all — in
  // both cases no picker renders and the offer's own count is declared verbatim, as before.
  const countSpec = spec?.count.type === "fixed" ? spec.count.data : null;
  // No client-side default: until the player types, the box shows the ENGINE's suggested count.
  const [picked, setPicked] = useState<string | null>(null);
  const raw = picked ?? (countSpec === null ? "" : String(countSpec.suggested));
  // `parseAmount` is the shared sanitization authority — it REJECTS out-of-window entries rather
  // than clamping, so a count the engine did not offer can never be declared.
  const chosen = countSpec === null ? null : parseAmount(raw, countSpec.min, countSpec.max);
  // CR 732.2a: each published element's `count` travels with its own magnitudes, so the match
  // is EXACT — no nearest-match, no interpolation, and nothing rendered for a count the engine
  // stated no magnitudes for.
  const previewed = chosen === null ? undefined : spec?.preview?.find((e) => e.count === chosen);

  // CR 732.2a + CR 601.2c: the declared count's partition across the announcement subjects
  // `choice_ids` names. The state is RAW STRINGS keyed by published choice id — the same shape the
  // count picker above uses, and for the same reason: `AmountInput` is controlled by a `raw` prop
  // and deliberately does not re-guard, so a parent holding numbers could only drop or coerce the
  // intermediate states a person types. `null` = no edit at the current count, so every row reads
  // the published allocation.
  const [authored, setAuthored] = useState<{ count: number; raw: Record<string, string> } | null>(
    null,
  );
  // CR 732.2c: an UntilLethal declaration announces ONE subject — the drive resolves it at every
  // repetition and never advances past it, so a longer list would certify choices nobody takes.
  // No default: `null` until the player selects, which is what disables Confirm.
  const [subject, setSubject] = useState<InteractionChoiceId | null>(null);
  // CR 603.5: the optional "may", whose choice is made on resolution; pinning it declares the same
  // answer for every iteration. No client-side default — an unanswered point disables Confirm.
  const [mayPicks, setMayPicks] = useState<Record<number, InteractionChoiceId>>({});

  const published: AmountAssignment[] = previewed?.allocation ?? [];
  const publishedRaw = (id: InteractionChoiceId) =>
    String(published.find((a) => a.choiceId === id)?.amount ?? 0);
  // The count tag travels with the edit: moving the picker moves `previewed`, this test goes
  // false, and an edit made at another count is DISCARDED rather than re-scaled.
  const rowRaw = (id: InteractionChoiceId) =>
    authored?.count === chosen ? (authored.raw[id] ?? publishedRaw(id)) : publishedRaw(id);

  // The declaration, re-parsed from what the rows actually READ, in published order. `parseAmount`
  // is the single sanitization authority here exactly as it is for the count, so an out-of-window
  // row is REFUSED (Confirm disables) rather than silently corrected.
  const allocationRows =
    targetsControl?.kind === "allocation" && chosen !== null
      ? targetsControl.point.candidateIds.map((id) => ({
          id,
          amount: parseAmount(rowRaw(id), 0, chosen),
        }))
      : [];
  const effective: AmountAssignment[] = allocationRows.every((r) => r.amount !== null)
    ? allocationRows.filter((r) => r.amount! > 0).map((r) => ({ choiceId: r.id, amount: r.amount! }))
    : [];
  const allocated = effective.reduce((sum, a) => sum + a.amount, 0);

  const editRow = (id: InteractionChoiceId, next: string) => {
    if (chosen === null) return;
    setAuthored({
      count: chosen,
      raw: Object.fromEntries(
        (targetsControl?.point.candidateIds ?? []).map((c) => [c, c === id ? next : rowRaw(c)]),
      ),
    });
  };

  // The player authored a split the selected element does not carry. Leading `pinRoute`
  // conjunct: off the route `showPreviewLines` reduces to `previewed !== undefined`, as a
  // property of the expression.
  const custom =
    pinRoute && targetsControl?.kind === "allocation" && !sameAllocation(effective, published);
  const showPreviewLines = previewed !== undefined && !custom;

  // Form completeness for button enablement only — summing the player's OWN declaration, never a
  // game consequence. The engine remains the sole legality authority.
  const declarationComplete =
    mayPoints.every((p) => mayPicks[p.group] !== undefined) &&
    (targetsControl?.kind !== "allocation" || (effective.length > 0 && allocated === chosen)) &&
    (targetsControl?.kind !== "subject" || subject !== null);

  const confirmDisabled =
    (countSpec !== null && chosen === null) || (pinRoute && !declarationComplete);

  // CR 732.2a + CR 601.2c: the declaration this offer currently states, built ONCE and read by
  // both consumers — the submission and the preview request — so what the player is shown and
  // what the player sends are the same object rather than two constructions that agree.
  const declaredResponse = (): InteractionResponse | null => {
    if (!pinRoute) return null;
    // CR 732.2a: a refused count entry has no count to declare. The `null` arm is the type-level
    // half of the same refusal — it is what lets `iterations` be the PARSED value rather than an
    // assertion, a default, a clamp or a fallback.
    const decision: InteractionShortcutDecision | null =
      spec.count.type !== "fixed"
        ? { type: "acceptSuggested" }
        : chosen === null
          ? null
          : { type: "fixed", data: { iterations: chosen } };
    if (decision === null) return null;

    // Only `targetsControl.point` can reach the `targets` arms — the group conjunct in
    // `renderable` is what guarantees it — so there is no second `targets` point for `effective`
    // to leak onto. `amounts` is always written explicitly. CR 732.2c: `null` on an unselected
    // subject is the same type-level refusal arm the count uses above, so the dispatched id is
    // the SELECTED value rather than an assertion, a default, a clamp or a fallback.
    const pinFor = (p: InteractionShortcutPoint): InteractionShortcutPin | null => {
      if (p.kind === "mayChoice") {
        // An unanswered group takes that same refusal arm, so an unset pick is unrepresentable
        // in a pin rather than shipped as `[undefined]`.
        const pick = mayPicks[p.group];
        return pick === undefined ? null : { group: p.group, choiceIds: [pick], amounts: [] };
      }
      return targetsControl?.kind === "allocation"
        ? { group: p.group, choiceIds: effective.map((a) => a.choiceId), amounts: effective }
        : subject === null
          ? null
          : { group: p.group, choiceIds: [subject], amounts: [] };
    };

    const pins = points.filter((p) => !p.readOnly).map(pinFor);
    if (pins.includes(null)) return null;

    return {
      type: "shortcut",
      data: {
        decision,
        pins: pins.filter((pin): pin is InteractionShortcutPin => pin !== null),
      },
    };
  };

  const handleConfirm = () => {
    // THE refusal, and the first statement of the ONE handler both production entry points reach:
    // the footer button's `onClick`, and every `AmountInput`'s Enter (`onSubmit`, which the box
    // calls unconditionally and deliberately does not re-guard). It reads the same predicate the
    // button's `disabled` reads, so the guard and the button state cannot drift.
    if (confirmDisabled) return;

    if (!pinRoute) {
      // The picker moves the COUNT only: `template` stays `null` because constructing a
      // `DecisionTemplate` is not a client authority.
      if (countSpec === null) {
        dispatch({
          type: "DeclareShortcut",
          data: { count: schema.iteration_count, template: null },
        });
        return;
      }
      // Runtime-redundant under the guard above, and load-bearing at the TYPE level: the compiler
      // cannot see that implication. The confirm button is disabled in the same state.
      if (chosen === null) return;
      dispatch({ type: "DeclareShortcut", data: { count: { Fixed: chosen }, template: null } });
      return;
    }

    const response = declaredResponse();
    if (response === null) return;

    const submission: InteractionSubmission = { interactionId: offerId, response };
    // `dispatchInteraction` already reports the error before rethrowing; the catch only
    // suppresses an unhandled rejection.
    void dispatchInteraction(submission).catch(() => undefined);
  };

  // CR 732.2a: the settled declaration stated as primitives, so one request is issued per SETTLED
  // edit rather than one per keystroke. `null` while there is nothing to preview.
  const declarationKey =
    custom && declarationComplete && offerId !== null
      ? [
          offerId,
          String(chosen),
          effective.map((a) => `${a.choiceId}:${a.amount}`).join(","),
          mayPoints.map((p) => `${p.group}:${mayPicks[p.group]}`).join(","),
        ].join("|")
      : null;

  const [answer, setAnswer] = useState<InteractionPreview | null>(null);
  const latest = useRef<PreviewRequestId | null>(null);
  const minted = useRef(0);

  useEffect(() => {
    // Load-bearing rather than cosmetic: the resolve guard below compares against the ANSWER's
    // echoed id, which a null answer does not carry, so this leading clear is the only thing that
    // drops a previous answer when no answer comes back.
    setAnswer(null);
    if (declarationKey === null || offerId === null) return;
    const response = declaredResponse();
    if (response === null) return;
    minted.current += 1;
    const requestId = `${offerId}.p${minted.current}` as PreviewRequestId;
    latest.current = requestId;
    const request: InteractionPreviewRequest = { requestId, interactionId: offerId, response };
    void previewInteractionResponse(request)
      .then((preview) => {
        if (latest.current === preview?.requestId) setAnswer(preview);
      })
      .catch(() => {
        if (latest.current === requestId) setAnswer(null);
      });
    // The settled-declaration key IS the dependency: it holds the primitives the request is built
    // from, and any wider identity would re-issue per keystroke.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [declarationKey]);

  // CR 732.2a: magnitudes are read off a CONFIRMABLE answer only. `?? null` collapses the
  // binding's optional-and-nullable spellings into the one absent state.
  const authoredPreview =
    answer?.status.type === "confirmable" ? (answer.shortcutPreview ?? null) : null;

  const handleDecline = useCallback(() => {
    // CR 732.2a: decline the auto-offer; the engine restores ordinary priority.
    dispatch({ type: "DeclineShortcut" });
  }, [dispatch]);

  // CR 702.51a: engine-computed count of untapped creatures the engine will auto-tap
  // for convoke — read directly from the schema (the engine owns the derivation).
  const convokeTappable = schema.convoke_tappable_count;

  const footer = (
    <div className="flex flex-col gap-3 sm:flex-row sm:justify-end">
      <button
        onClick={handleConfirm}
        disabled={confirmDisabled}
        className={`min-h-11 rounded-[16px] bg-cyan-500 px-6 py-2 font-semibold text-slate-950 shadow-[0_14px_34px_rgba(6,182,212,0.28)] transition hover:bg-cyan-400 ${
          confirmDisabled ? "cursor-not-allowed opacity-50 hover:bg-cyan-500" : ""
        }`}
      >
        {t("comboShortcut.confirm")}
      </button>
      {/* CR 732.2a: declining is offered only when the engine says this offer may be declined. */}
      {(spec?.allowDecline ?? true) && (
        <button
          onClick={handleDecline}
          className="min-h-11 rounded-[16px] border border-white/8 bg-white/5 px-6 py-2 font-semibold text-slate-200 transition hover:bg-white/8"
        >
          {t("comboShortcut.decline")}
        </button>
      )}
    </div>
  );

  return (
    <DialogShell
      title={t("comboShortcut.declareTitle")}
      subtitle={t("comboShortcut.declareSubtitle")}
      size="md"
      footer={footer}
    >
      <div className="flex flex-col gap-3 px-3 py-3 lg:px-5 lg:py-5">
        <WinKindLine kind={certificate.win_kind} />
        <CountLine count={schema.iteration_count} />
        {countSpec && (
          <AmountInput
            raw={raw}
            onRawChange={setPicked}
            min={countSpec.min}
            max={countSpec.max}
            onSubmit={handleConfirm}
            labels={{
              input: t("comboShortcut.countAria"),
              // Several amount controls stand side by side in this dialog, so a stepper names
              // the quantity it steps rather than taking `AmountInput`'s shared
              // `mana.decreaseAmount`.
              decrease: t("comboShortcut.countDecreaseAria"),
              increase: t("comboShortcut.countIncreaseAria"),
            }}
          />
        )}
        {pinRoute && targetsControl?.kind === "allocation" && chosen !== null && (
          <div className="flex flex-col gap-2 rounded-lg bg-white/5 px-3 py-2">
            <p className="text-xs font-semibold tracking-wide text-slate-400 uppercase">
              {t("comboShortcut.allocationTitle")}
            </p>
            {targetsControl.point.candidateIds.map((id) => {
              const subject = candidateLabel(t, candidates, id);
              return (
                <SubjectControl key={id} subject={subject}>
                  <AmountInput
                    raw={rowRaw(id)}
                    onRawChange={(next) => editRow(id, next)}
                    min={0}
                    max={chosen}
                    onSubmit={handleConfirm}
                    labels={{
                      input: t("comboShortcut.allocationAria", { subject }),
                      decrease: t("comboShortcut.allocationDecreaseAria", { subject }),
                      increase: t("comboShortcut.allocationIncreaseAria", { subject }),
                    }}
                  />
                </SubjectControl>
              );
            })}
            <p className="text-xs text-slate-400 tabular-nums">
              {t("comboShortcut.allocationSum", { allocated, total: chosen })}
            </p>
            <button
              onClick={() => setAuthored(null)}
              className="min-h-9 self-start rounded-[12px] border border-white/8 bg-white/5 px-3 py-1 text-sm font-semibold text-slate-200 transition hover:bg-white/8"
            >
              {t("comboShortcut.evenSplit")}
            </button>
          </div>
        )}
        {pinRoute && targetsControl?.kind === "subject" && (
          <div className="flex flex-col gap-2 rounded-lg bg-white/5 px-3 py-2">
            <p className="text-xs font-semibold tracking-wide text-slate-400 uppercase">
              {t("comboShortcut.announceTitle")}
            </p>
            {/* The domain is the point's published `candidateIds` and the bound is its published
                `max`; the modal derives neither. */}
            {targetsControl.point.candidateIds.map((id) => {
              const label = candidateLabel(t, candidates, id);
              const picked = subject === id;
              return (
                <SubjectControl key={id} subject={label}>
                  <button
                    onClick={() => setSubject(id)}
                    aria-pressed={picked}
                    aria-label={t("comboShortcut.announceAria", { subject: label })}
                    className={`min-h-9 rounded-[12px] border px-3 py-1 text-sm font-semibold transition ${
                      picked
                        ? "border-cyan-400/60 bg-cyan-500/20 text-cyan-100"
                        : "border-white/8 bg-white/5 text-slate-200 hover:bg-white/8"
                    }`}
                  >
                    {t("comboShortcut.announce")}
                  </button>
                </SubjectControl>
              );
            })}
          </div>
        )}
        {mayPoints.map((p, index) => {
          // The ordinal IS the subject: nothing else this panel renders distinguishes one may
          // point from another. It counts the RENDERED panels, not the published points, so it
          // can differ from `group`; `group` still keys `mayPicks`, the React key and the pin, so
          // the wire payload stays the engine's. The SAME ordinal feeds the accessible names
          // below, so what a player reads and what a screen reader says cannot disagree.
          const ordinal = index + 1;
          return (
            <div key={p.group} className="flex flex-col gap-2 rounded-lg bg-white/5 px-3 py-2">
              <SubjectControl subject={t("comboShortcut.mayTitle", { group: ordinal })}>
                <div className="flex flex-wrap gap-2">
                  {p.candidateIds.map((id) => {
                    const take = mayCandidate(candidates, id) === "take";
                    const picked = mayPicks[p.group] === id;
                    return (
                      <button
                        key={id}
                        onClick={() => setMayPicks((prev) => ({ ...prev, [p.group]: id }))}
                        aria-pressed={picked}
                        aria-label={t(
                          take ? "comboShortcut.mayTakeAria" : "comboShortcut.mayDeclineAria",
                          { group: ordinal },
                        )}
                        className={`min-h-9 rounded-[12px] border px-3 py-1 text-sm font-semibold transition ${
                          picked
                            ? "border-cyan-400/60 bg-cyan-500/20 text-cyan-100"
                            : "border-white/8 bg-white/5 text-slate-200 hover:bg-white/8"
                        }`}
                      >
                        {t(take ? "comboShortcut.mayTake" : "comboShortcut.mayDecline")}
                      </button>
                    );
                  })}
                </div>
              </SubjectControl>
            </div>
          );
        })}
        {showPreviewLines && previewed && <PreviewLines preview={previewed} />}
        {custom &&
          // `PreviewLines` states nothing for an element carrying no magnitudes, so the ENTRY
          // COUNT is the predicate here: an answer without one still states the landed split.
          (authoredPreview?.entries.length ? (
            <PreviewLines preview={authoredPreview} />
          ) : (
            <p className="text-sm text-slate-300">{t("comboShortcut.customDistribution")}</p>
          ))}
        {/* Outside the `showPreviewLines` gate deliberately: the invariant families state a family
            and no magnitude, so they survive a custom distribution. */}
        <FamilyBadges axes={certificate.unbounded} />
        {convokeTappable > 0 && (
          <p className="text-xs text-slate-400">
            {t("comboShortcut.convokeInfo", { count: convokeTappable })}
          </p>
        )}
      </div>
    </DialogShell>
  );
}

/**
 * CR 732.2b/c: after the proposer declares, each other living player, in APNAP
 * order, may accept the shortcut or shorten it (break out to resume manual play).
 * Phase 3 discards `at_iteration` (no finite-K materialization), so "Break out"
 * dispatches a placeholder `at_iteration: 1`.
 */
export function RespondToShortcutModal() {
  const { t } = useTranslation("game");
  const canAct = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);
  // Both selectors return a reference INTO store state (or null), so both are `Object.is`-stable.
  const spec = useGameStore((s) => shortcutReplySpec(s.viewerInteraction));
  const publishedCandidates = useGameStore((s) => shortcutReplyCandidates(s.viewerInteraction));

  const handleAccept = useCallback(() => {
    dispatch({ type: "RespondToShortcut", data: { response: "Accept" } });
  }, [dispatch]);

  const handleShorten = useCallback(() => {
    dispatch({ type: "RespondToShortcut", data: { response: { Shorten: { at_iteration: 1 } } } });
  }, [dispatch]);

  if (waitingFor?.type !== "RespondToShortcut" || !canAct) return null;

  const { proposal } = waitingFor.data;
  const candidates = publishedCandidates ?? [];
  // CR 732.2b: everything below is a direct read of a published field. The count, the partition,
  // every per-seat magnitude and every answer are the engine's; this modal states them.
  const declared = spec?.declared ?? null;
  const points = spec?.points ?? [];
  // CR 601.2c: a proposal may carry MORE THAN ONE announced-target decision. The engine names
  // the group its allocation is stated over; that decision's order is already the allocation
  // lines' own order, so it is the one dropped here, and it is dropped by NAME rather than by
  // position. No group is named exactly when no allocation is stated, and then every decision
  // states its order — reading only the first would show the responder half the proposal.
  const allocationGroup = spec?.allocationGroup ?? null;
  const orderPoints = points.filter((p) => p.kind === "targets" && p.group !== allocationGroup);
  const allocation = declared?.allocation ?? [];
  // One authority for whether a may row exists, so the panel's predicate and its render cannot
  // drift: a point this modal has no wording for contributes neither a row nor a title.
  const mayRows = points
    .filter((p) => p.kind === "mayChoice")
    .flatMap((point) => {
      // The engine publishes EXACTLY TWO candidate ids on a `mayChoice` statement point,
      // read in order as SUBJECT then ANSWER; a decision whose subject cannot be minted
      // publishes no point at all, so this positional read is total over what arrives.
      const [subjectId, answerId] = point.candidateIds;
      if (subjectId === undefined || answerId === undefined) return [];
      const answer = mayCandidate(candidates, answerId);
      // A whitelist, deliberately: an answer this modal has no wording for renders
      // nothing rather than a raw lookup key.
      if (answer !== "take" && answer !== "decline") return [];
      return [
        <p key={point.group} className="text-sm text-slate-200">
          {t(`comboShortcut.respondDecision.${answer}`, {
            subject: candidateLabel(t, candidates, subjectId),
          })}
        </p>,
      ];
    });
  const showsDeclaration =
    allocation.length > 0 ||
    orderPoints.some((p) => p.candidateIds.length > 0) ||
    mayRows.length > 0;

  const footer = (
    <div className="flex flex-col gap-3 sm:flex-row sm:justify-end">
      <button
        onClick={handleAccept}
        className="min-h-11 rounded-[16px] bg-cyan-500 px-6 py-2 font-semibold text-slate-950 shadow-[0_14px_34px_rgba(6,182,212,0.28)] transition hover:bg-cyan-400"
      >
        {t("comboShortcut.accept")}
      </button>
      <button
        onClick={handleShorten}
        className="min-h-11 rounded-[16px] border border-white/8 bg-white/5 px-6 py-2 font-semibold text-slate-200 transition hover:bg-white/8"
      >
        {t("comboShortcut.shorten")}
      </button>
    </div>
  );

  return (
    <DialogShell
      title={t("comboShortcut.respondTitle")}
      subtitle={t("comboShortcut.respondSubtitle")}
      size="md"
      footer={footer}
    >
      <div className="flex flex-col gap-3 px-3 py-3 lg:px-5 lg:py-5">
        <WinKindLine kind={proposal.win_kind} />
        <CountLine count={proposal.count} />
        <FamilyBadges axes={proposal.unbounded} />
        {declared && <PreviewLines preview={declared} />}
        {showsDeclaration && (
          <div className="flex flex-col gap-1 rounded-lg bg-white/5 px-3 py-2">
            <p className="text-xs font-semibold tracking-wide text-slate-400 uppercase">
              {t("comboShortcut.respondDeclaredTitle")}
            </p>
            {allocation.map((entry) => (
              <p key={entry.choiceId} className="text-sm text-slate-200 tabular-nums">
                {t("comboShortcut.respondAllocationEntry", {
                  repetitions: entry.amount,
                  subject: candidateLabel(t, candidates, entry.choiceId),
                })}
              </p>
            ))}
            {/* Positions are numbered WITHIN their own announced decision, which is the only
                thing the engine states them over; the key carries the group so two decisions
                naming the same subject stay distinct rows. */}
            {orderPoints.flatMap((point) =>
              point.candidateIds.map((id, index) => (
                <p
                  key={`${point.group}:${id}`}
                  className="text-sm text-slate-200 tabular-nums"
                >
                  {t("comboShortcut.respondOrderEntry", {
                    position: index + 1,
                    subject: candidateLabel(t, candidates, id),
                  })}
                </p>
              )),
            )}
            {mayRows}
          </div>
        )}
      </div>
    </DialogShell>
  );
}
