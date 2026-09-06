/**
 * Pure view-model derivations for the tournament-organizer surface.
 *
 * This module is the single authority for every derivation the tournament
 * components need. It is pure: no React, no store runtime, no I/O, no clock.
 * It renders nothing and it *formats* only — it never re-sorts, re-ranks or
 * re-computes anything the broker already decided. Standings arrive
 * pre-ranked (`crates/lobby-broker/src/tournament.rs:905-935`), pairings
 * arrive in generation order, and arity/bracket legality is validated
 * server-side alone.
 *
 * **Every import here is `import type`, by design.** The store's runtime
 * touches `localStorage` through its persistence middleware; importing a
 * *value* from it drags that runtime into every consumer of this module.
 * Measured on this tree: a type-only import costs 14ms of import time, the
 * same import switched to a value import costs 925ms *and* materializes the
 * Node `--localstorage-file` hazard. `verbatimModuleSyntax` makes the erasure
 * a compiler guarantee, and a static source assertion in
 * `__tests__/tournamentPageState.test.ts` pins it.
 *
 * Three of the exports below (`outcomeLabelKey`, `isReportable`,
 * `decisiveGameWins`) are independent exhaustive walks over the same
 * `PairingOutcome` wire union, and `tiebreakCells` is a fourth over
 * `Tiebreaks`. Each terminates in a `const unreachable: never` binding rather
 * than a `default:` arm, so a fifth wire arm fails the build in every place
 * that must make a decision about it. Components never discriminate these
 * unions themselves; they call these functions. `failureLabel` is a further
 * exhaustive walk of the same form, over the failure-reason union rather than
 * a wire union.
 */

import type {
  MatchArity,
  PairingOutcome,
  PlayerSummary,
  PodOutcome,
  ScoringPolicy,
  Tiebreaks,
  TournamentPairingView,
  TournamentView,
} from "../adapter/types";
import type {
  GatedTournamentRpcResult,
  TournamentCredential,
  TournamentRole,
} from "../stores/multiplayerStore";

/**
 * The authorities this browser holds for one tournament.
 *
 * Returns a set rather than a boolean because an organizer may also join
 * their own event — `CreateTournament` does not auto-join the creator, so a
 * playing organizer holds BOTH tokens under one code, and that is the normal
 * path rather than an exotic one (`stores/multiplayerStore.ts`'s
 * `TournamentCredential` doc comment). A `boolean` cannot express it, and a
 * pair of booleans would be the sibling-cluster smell CLAUDE.md names. The
 * member type is the store's own {@link TournamentRole}, reused rather than
 * re-declared, so display authority and action authority share one vocabulary.
 *
 * This reads the credential map and nothing else — never a `TournamentView`.
 * `runGatedTournamentRpc` is the single authority for *acting*; this is the
 * single authority for *displaying*, off the same map, so the two can never
 * disagree.
 */
export function viewerRoles(
  credential: TournamentCredential | undefined,
): ReadonlySet<TournamentRole> {
  const roles = new Set<TournamentRole>();
  if (credential?.organizerToken !== undefined) roles.add("organizer");
  if (credential?.playerToken !== undefined) roles.add("player");
  return roles;
}

/**
 * A catalog key for a resolved pairing outcome, carried together with the
 * interpolation variable that key needs.
 *
 * Key and vars travel as one value so "called a key without its variable" is
 * unrepresentable: two of the four keys interpolate `{{winner}}`, and a
 * consumer narrows with `"winner" in label` rather than remembering which.
 */
export type OutcomeLabel =
  | { readonly key: "outcome.bye" }
  | { readonly key: "outcome.draw" }
  | { readonly key: "outcome.forfeit"; readonly winner: string }
  | { readonly key: "outcome.decisive"; readonly winner: string };

/**
 * Resolves a `player_key` to the display name carried by the same frame.
 *
 * Falls back to the raw key rather than to a blank: an outcome naming a seat
 * that is not in `seats` is a wire inconsistency, and rendering nothing would
 * hide it. A raw key is ugly and diagnosable; an empty cell is neither.
 */
function displayNameFor(
  seats: readonly PlayerSummary[],
  playerKey: string,
): string {
  return (
    seats.find((seat) => seat.player_key === playerKey)?.display_name ??
    playerKey
  );
}

/**
 * The catalog key (and interpolation variable) for a pairing's resolved
 * outcome.
 *
 * **Never construct one of these keys from a wire tag.** The catalog keys are
 * lowercase (`outcome.bye`, `outcome.draw`) while the wire tags are `"Bye"`
 * and `"Draw"`, so `t(\`outcome.${tag.toLowerCase()}\`)` *appears* to work for
 * those two — and then silently breaks for forfeit and decisive, which are not
 * 1:1 with wire tags at all (`Reported` wraps `Decisive`/`Draw` at a second
 * level). Measured: `i18n.exists("tournament:outcome.Bye")` is `false`. The
 * exhaustive walk below is the whole point.
 */
export function outcomeLabelKey(
  outcome: PairingOutcome,
  seats: readonly PlayerSummary[],
): OutcomeLabel {
  if (outcome === "Bye") return { key: "outcome.bye" };
  if ("Forfeit" in outcome) {
    return {
      key: "outcome.forfeit",
      winner: displayNameFor(seats, outcome.Forfeit.winner),
    };
  }
  if ("Reported" in outcome) {
    const reported: PodOutcome = outcome.Reported;
    if (reported === "Draw") return { key: "outcome.draw" };
    if ("Decisive" in reported) {
      return {
        key: "outcome.decisive",
        winner: displayNameFor(seats, reported.Decisive.winner),
      };
    }
    const unreachablePod: never = reported;
    return unreachablePod;
  }
  const unreachable: never = outcome;
  return unreachable;
}

/**
 * Whether the broker will accept a `ReportMatchResult` for this pairing at
 * all — a *total* contract, not a validity judgement about a submission's
 * contents (which is `validate_match_result`'s alone, and is never
 * duplicated here).
 *
 * `TournamentMeta::report_result`
 * (`crates/lobby-broker/src/tournament.rs:1741-1753`) matches the pairing's
 * existing outcome and returns `Err` for two arms *before*
 * `validate_match_result` is ever reached (`:1754`):
 *   - `Bye`     (`:1742-1746`) — "is a bye and has no result to report"
 *   - `Forfeit` (`:1747-1751`) — "was resolved by forfeit and cannot be reported"
 * Both are production-reachable: byes come from `partition_round`'s ordinary
 * remainder handling (`:1323`) and forfeits from `drop_player`'s
 * auto-settlement of a pairing left with one active player (`:1828`).
 *
 * `Some(PairingOutcome::Reported(_)) | None => {}` (`:1752`) means an
 * ALREADY-REPORTED pairing may be reported AGAIN — a reported result is
 * overwritten at `:1755`, and correcting a mistyped tally is a legitimate
 * organizer action. This guard is therefore **arm-selective, not
 * "unresolved only"**: narrowing it to `outcome === null` would wrongly hide
 * a legal affordance.
 *
 * Predicate form (rather than a typed result) mirrors the broker's own
 * `TournamentStatus::is_terminal()` guard, called earlier in this same
 * function (`:1730`; `report_result` opens at `:1721`).
 *
 * This answers "can this pairing be reported by anyone", NOT "may this viewer
 * report it". Authorization is a separate, orthogonal guard on the caller's
 * side, and it is *three* conjuncts, mirroring the broker's own three
 * refusals for a report — `authorize_player`'s token and dropped checks
 * plus `handle_report_match_result`'s seat check
 * (`crates/lobby-broker/src/broker.rs`): {@link viewerRoles} — is this viewer
 * a player at all; {@link isActiveEntrant} — has that player not dropped; and
 * {@link myPairing} — is that player seated in THIS pairing. This arm gate
 * plus all three are required; none of the four is sufficient alone.
 */
export function isReportable(outcome: PairingOutcome | null): boolean {
  if (outcome === null) return true; // pending — the broker's `None` arm
  if (outcome === "Bye") return false; // tournament.rs:1742-1746
  if ("Forfeit" in outcome) return false; // tournament.rs:1747-1751
  if ("Reported" in outcome) return true; // tournament.rs:1752 — re-reporting is legal
  const unreachable: never = outcome;
  return unreachable;
}

/**
 * One tiebreak column, projected from whichever {@link Tiebreaks} arm the
 * broker chose for this row.
 */
export interface TiebreakCell {
  /**
   * Scheme-qualified, e.g. `"headToHead.gameWinPct"`. The qualification is
   * load-bearing: both arms carry an `opponentsMatchWinPct`, so an
   * unqualified id would let a `Multiplayer` row's value render under a
   * `HeadToHead` header — asserting an equivalence the client has no
   * authority to assert, and doing it silently.
   */
  readonly id: string;
  /** Full catalog key for the column header, e.g. `"standings.tiebreaks.headToHead.gameWinPct"`. */
  readonly labelKey: string;
  /** Full catalog key for the header's `title` attribute. */
  readonly titleKey: string;
  /** Server-computed. Never re-derived here. */
  readonly value: number;
  readonly format: "percent" | "points";
}

/**
 * The tiebreak columns for one standings row, in the order they rank.
 *
 * `HeadToHead` is MTR §3.1's order; `Multiplayer` is MSTR's
 * (`crates/lobby-broker/src/tournament.rs:714-726`). Which arm a row carries
 * is the broker's decision — this reads it, and never chooses it.
 */
export function tiebreakCells(tiebreaks: Tiebreaks): readonly TiebreakCell[] {
  if ("HeadToHead" in tiebreaks) {
    const h2h = tiebreaks.HeadToHead;
    return [
      {
        id: "headToHead.opponentsMatchWinPct",
        labelKey: "standings.tiebreaks.headToHead.opponentsMatchWinPct",
        titleKey: "standings.tiebreaks.headToHead.opponentsMatchWinPctTitle",
        value: h2h.opponents_match_win_pct,
        format: "percent",
      },
      {
        id: "headToHead.gameWinPct",
        labelKey: "standings.tiebreaks.headToHead.gameWinPct",
        titleKey: "standings.tiebreaks.headToHead.gameWinPctTitle",
        value: h2h.game_win_pct,
        format: "percent",
      },
      {
        id: "headToHead.opponentsGameWinPct",
        labelKey: "standings.tiebreaks.headToHead.opponentsGameWinPct",
        titleKey: "standings.tiebreaks.headToHead.opponentsGameWinPctTitle",
        value: h2h.opponents_game_win_pct,
        format: "percent",
      },
    ];
  }
  if ("Multiplayer" in tiebreaks) {
    const mp = tiebreaks.Multiplayer;
    return [
      {
        id: "multiplayer.matchWinPct",
        labelKey: "standings.tiebreaks.multiplayer.matchWinPct",
        titleKey: "standings.tiebreaks.multiplayer.matchWinPctTitle",
        value: mp.match_win_pct,
        format: "percent",
      },
      {
        id: "multiplayer.opponentsAvgMatchPoints",
        labelKey: "standings.tiebreaks.multiplayer.opponentsAvgMatchPoints",
        titleKey: "standings.tiebreaks.multiplayer.opponentsAvgMatchPointsTitle",
        value: mp.opponents_avg_match_points,
        format: "points",
      },
      {
        id: "multiplayer.opponentsMatchWinPct",
        labelKey: "standings.tiebreaks.multiplayer.opponentsMatchWinPct",
        titleKey: "standings.tiebreaks.multiplayer.opponentsMatchWinPctTitle",
        value: mp.opponents_match_win_pct,
        format: "percent",
      },
    ];
  }
  const unreachable: never = tiebreaks;
  return unreachable;
}

/**
 * Display formatting for a server-computed tiebreak value. Pure presentation
 * of a number the broker already decided — the value itself is never derived
 * here.
 *
 * One decimal place on percentages is enough to break visible ties without
 * implying precision the `f64` does not carry meaningfully. That is a display
 * decision, not a rules claim.
 */
export function formatTiebreakValue(cell: TiebreakCell): string {
  if (cell.format === "percent") return `${(cell.value * 100).toFixed(1)}%`;
  if (cell.format === "points") return cell.value.toFixed(2);
  const unreachable: never = cell.format;
  return unreachable;
}

/**
 * The game-wins tally carried by a pairing's outcome, or `null` when the
 * outcome carries none. The single place that reaches into
 * `PairingOutcome`'s nested `Reported -> Decisive` shape: components call
 * this and null-check the result, and never narrow the union themselves.
 *
 * Four of the five reachable states have no tally, each for its own reason
 * and none of them an error:
 *   - `null`               pending; nothing reported yet
 *   - `"Bye"`              server-assigned (tournament.rs:1323), never played
 *   - `{Forfeit}`          server-assigned by `drop_player` (:1828)
 *   - `{Reported:"Draw"}`  MSTR: all seated players draw; no per-seat wins
 *
 * Only `{Reported:{Decisive:{game_wins}}}` yields a record — and that record
 * is legitimately EMPTY at three or more seats, because pods are
 * single-game per MSTR and `validate_match_result`
 * (`crates/lobby-broker/src/tournament.rs:967-1021`) rejects any non-empty
 * map there ("Pod results are single-game per MSTR - game_wins must be
 * empty", `:1015`).
 *
 * `{}` and `null` are therefore DIFFERENT FACTS and must never be collapsed:
 * `{}` means "a decisive result with no per-game tally to show"; `null`
 * means "there is no decisive result here at all".
 */
export function decisiveGameWins(
  outcome: PairingOutcome | null,
): Readonly<Record<string, number>> | null {
  if (outcome === null) return null; // pending
  if (outcome === "Bye") return null; // tournament.rs:1323
  if ("Forfeit" in outcome) return null; // tournament.rs:1828
  if ("Reported" in outcome) {
    const reported: PodOutcome = outcome.Reported;
    if (reported === "Draw") return null; // MSTR: no per-seat tally
    if ("Decisive" in reported) return reported.Decisive.game_wins;
    const unreachablePod: never = reported;
    return unreachablePod;
  }
  const unreachable: never = outcome;
  return unreachable;
}

/** One seat's game-win count, resolved to that seat's display name. */
export interface GameWinEntry {
  readonly playerKey: string;
  readonly name: string;
  readonly wins: number;
}

/**
 * Joins a game-wins tally onto the pairing's seats, **in seat order**.
 *
 * The seat order is the authority, not the record's own key order, and that
 * is not merely stylistic. `player_key` is client-supplied and opaque
 * (`crates/lobby-broker/src/protocol.rs:699-702`), so an all-digit key is
 * legal — and JavaScript hoists integer-like keys to the front of an object
 * in ascending numeric order. Measured:
 * `Object.keys(JSON.parse('{"12":2,"7":0,"alice":1}'))` is
 * `["7","12","alice"]`, which is not the order the seats were paired in.
 * Iterating `seats` is the only stable authority.
 *
 * Keys matching no seat are dropped: they cannot be placed in seat order, and
 * the broker rejects such payloads at write time anyway
 * (`game_wins.len() != 2 || !contains_key(a) || !contains_key(b)`,
 * `crates/lobby-broker/src/tournament.rs:967-1021`).
 *
 * This function discriminates no union — it is handed a record. Producing
 * that record from a `PairingOutcome` is {@link decisiveGameWins}'s job, and
 * keeping the two separate is deliberate: `{}` ("decisive, but a pod") and
 * `null` ("no decisive result at all") stay distinguishable to the caller.
 */
export function gameWinsEntries(
  gameWins: Readonly<Record<string, number>>,
  seats: readonly PlayerSummary[],
): readonly GameWinEntry[] {
  return seats
    .filter((seat) =>
      Object.prototype.hasOwnProperty.call(gameWins, seat.player_key),
    )
    .map((seat) => ({
      playerKey: seat.player_key,
      name: seat.display_name,
      wins: gameWins[seat.player_key],
    }));
}

/**
 * This browser's pairing in the tournament's **current** round, or `null`.
 *
 * The round conjunct is load-bearing: `pairings` is a full history, never a
 * filtered subset, so dropping it would return a stale earlier-round pairing
 * for anyone who has since been paired differently — or, during Registration
 * (`current_round === 0`), a pairing that has not been created yet.
 *
 * `playerKey` comes from the store's `TournamentCredential.playerKey`, which
 * is stored beside the token precisely so "which entrant am I in THIS event"
 * survives an ambient-id change. A spectator holds none, and gets `null`.
 */
export function myPairing(
  view: TournamentView,
  playerKey: string | undefined,
): TournamentPairingView | null {
  if (playerKey === undefined) return null;
  return (
    view.pairings.find(
      (pairing) =>
        pairing.round === view.summary.current_round &&
        pairing.players.some((seat) => seat.player_key === playerKey),
    ) ?? null
  );
}

/** A catalog key describing a tournament's match arity, with its vars. */
export type ArityLabel =
  | { readonly key: "arity.headToHead" }
  | { readonly key: "arity.pod"; readonly seats: number };

/**
 * How to describe a tournament's match arity. `2` is head-to-head; anything
 * else is a pod of that many seats (`MatchArity::new` admits `2..=128`,
 * `crates/lobby-broker/src/tournament.rs:96-113`).
 */
export function arityLabel(arity: MatchArity): ArityLabel {
  if (arity === 2) return { key: "arity.headToHead" };
  return { key: "arity.pod", seats: arity };
}

/**
 * The scoring policy to **prefill** a creation form with, for a given arity.
 *
 * Prefill only. Mirrors `ScoringPolicy::default_for_arity`
 * (`crates/lobby-broker/src/tournament.rs:217-227`) because
 * `CreateTournament.scoring` is wire-mandatory and has no `#[serde(default)]`
 * (`crates/lobby-broker/src/protocol.rs:692`) and no RPC exposes the broker's
 * default. The organizer may edit the result and the broker validates it
 * (`ScoringPolicy::new` rejects `win_points == 0`), so drift here degrades a
 * default, never a guarantee.
 *
 * Arity-dependent by design: a fixed 3/1/0 would silently give every pod
 * organizer MTR head-to-head scoring instead of MSTR pod scoring.
 */
export function defaultScoringForArity(arity: MatchArity): ScoringPolicy {
  return { win_points: 2 * arity - 1, draw_points: 1, loss_points: 0 };
}

/**
 * How this browser relates to one tournament, for display.
 *
 * Three members, 1:1 with the catalog keys `labels.organizer` /
 * `labels.entered` / `labels.spectating`, so `t(\`labels.${relation}\`)` is a
 * direct index — the same licensed pattern as `status.*` and `bracket.*`
 * (flat unions whose members ARE the key leaves), and NOT the forbidden
 * `outcome.*` pattern (whose keys are not 1:1 with wire tags).
 */
export type ViewerRelation = "organizer" | "entered" | "spectating";

/**
 * The relation to render as a badge for a viewer holding `roles`.
 *
 * Organizer-dominant precedence, because a playing organizer holds BOTH
 * tokens under one code and that is the normal path rather than an exotic one
 * (`CreateTournament` does not auto-join the creator). Two badges on one row
 * is noise; the organizer relation is the stronger claim and subsumes the
 * weaker one for display.
 *
 * **This is a display precedence only — it must never be read as an authority
 * decision.** Authority stays with {@link viewerRoles} (a *set*, precisely
 * because a boolean cannot express the both-tokens case) and, for acting, with
 * the store's `runGatedTournamentRpc` alone. In particular, a viewer resolving
 * to `"organizer"` here may still hold a player token, and one resolving to
 * `"entered"` may have dropped — see {@link isActiveEntrant}.
 */
export function viewerRelation(
  roles: ReadonlySet<TournamentRole>,
): ViewerRelation {
  if (roles.has("organizer")) return "organizer";
  if (roles.has("player")) return "entered";
  return "spectating";
}

/**
 * A catalog key for a failed tournament action, carried with the
 * interpolation variable that key needs. Key and vars travel as one value so
 * "called a key without its variable" is unrepresentable — the same shape as
 * {@link OutcomeLabel} and {@link ArityLabel} in this module.
 */
export type FailureLabel =
  | { readonly key: "errors.notOrganizer" }
  | { readonly key: "errors.notEntered" }
  | { readonly key: "errors.timedOut" }
  | { readonly key: "errors.connectionLost" }
  | { readonly key: "errors.aborted" }
  | { readonly key: "errors.unsupported" }
  | { readonly key: "errors.serverRejected"; readonly message: string };

/** The failure half of a gated action's result — also total over an ungated one. */
type TournamentFailure = Extract<
  GatedTournamentRpcResult<unknown>,
  { ok: false }
>;

/**
 * The copy to show for a failed tournament action.
 *
 * The `not_authorized` arm reads the store's **typed** `role` discriminator,
 * never the English `message`: `TournamentNotAuthorized` exists precisely
 * because a local refusal is undecidable from a wire
 * `{ok:false, reason:"rejected"}`, and this is the single consumer that
 * discriminator was built for. The `rejected` arm carries the broker's own
 * text through **verbatim and untranslated** — `"the broker answered `Error`;
 * `message` is its text verbatim"` (`services/tournamentClient.ts`) — which is
 * why `errors.serverRejected` interpolates `{{message}}` rather than replacing
 * it.
 *
 * `errors.notFound` is deliberately **not** in this map: no wire reason
 * produces it. It is a client-known fact — a `TournamentRemoved` broadcast for
 * the code currently being viewed — and is rendered by the detail page alone.
 *
 * Terminates in a `const unreachable: never` binding and has no `default:`
 * arm, so any new failure member (another `TournamentRpcFailureReason` member,
 * or a second store-level refusal) fails the build here rather than rendering
 * a blank alert.
 *
 * The `unsupported` arm carries no variable on purpose: it reports that the
 * request went out against a broker that cannot confirm it, and there is no
 * broker text to pass through, because the broker was never asked to explain
 * anything. Its copy must say "sent, not confirmed" rather than implying the
 * action failed.
 *
 * The terminal binds the **discriminant** (`failure.reason`) rather than the
 * value, and that is forced rather than stylistic: `TournamentFailure` is not
 * a flat discriminated union. Its wire member declares
 * `reason: TournamentRpcFailureReason` — a string-literal union *inside one
 * object type* — and TypeScript's narrowing can only eliminate whole union
 * members, never shrink a property union in place, so `failure` itself is
 * still that member after every arm and `const unreachable: never =
 * failure` does not compile. Narrowing the discriminant reference does work,
 * and it is the same gate: any new `reason` literal, from either member, lands
 * here as a `never` assignment error.
 */
export function failureLabel(failure: TournamentFailure): FailureLabel {
  if (failure.reason === "not_authorized") {
    if (failure.role === "organizer") return { key: "errors.notOrganizer" };
    if (failure.role === "player") return { key: "errors.notEntered" };
    // Nested terminal over the role union, mirroring `unreachablePod` above.
    const unreachableRole: never = failure.role;
    return unreachableRole;
  }
  if (failure.reason === "rejected") {
    return { key: "errors.serverRejected", message: failure.message };
  }
  if (failure.reason === "timeout") return { key: "errors.timedOut" };
  if (failure.reason === "connection_lost") {
    return { key: "errors.connectionLost" };
  }
  if (failure.reason === "aborted") return { key: "errors.aborted" };
  if (failure.reason === "unsupported") return { key: "errors.unsupported" };
  const unreachable: never = failure.reason;
  return unreachable;
}

/**
 * Whether `playerKey` names an entrant of `view` who has **not** dropped.
 *
 * This is the client-side mirror of the second conjunct `authorize_player`
 * enforces (`crates/lobby-broker/src/broker.rs`): a token that resolves to an
 * entrant is refused anyway once `player.dropped` is true
 * (`"Player has dropped from tournament {code}"`). Token possession —
 * {@link viewerRoles} — answers the *first* conjunct only, and the store
 * clears a credential on `TournamentRemoved` alone, never on a drop, so
 * `roles.has("player")` stays true forever after a drop. A gate written on
 * possession alone therefore renders an affordance the broker refuses every
 * time; both player affordances need this conjunct too.
 *
 * Positive polarity on purpose: callers write
 * `roles.has("player") && isActiveEntrant(...)`, a plain conjunction, so no
 * edit can silently lose a `!`.
 *
 * Fails closed in both directions. An absent `playerKey` (a spectator) is not
 * active. A `playerKey` absent from `view.players` is not active either —
 * and that is sound rather than merely cautious, because `players` is a full
 * history that keeps dropped entrants listed (`adapter/types.ts`'s
 * `TournamentView` doc: *"dropped players stay listed (their `dropped` flag
 * is the distinction to render)"*), so absence means "not an entrant of the
 * tournament this view describes" — a foreign or not-yet-seeded view — never
 * "an entrant whose row was filtered out". If the broker ever did start
 * filtering dropped entrants out of `players`, this still answers `false`,
 * which is still the correct gate.
 *
 * "Active entrant" is the wire's own vocabulary for exactly this predicate,
 * not a coinage: `TournamentSummary.player_count` is documented as
 * "**Active** entrants — `TournamentMeta::active_player_count`", i.e. the
 * count of entrants for which this function is true.
 */
export function isActiveEntrant(
  view: TournamentView,
  playerKey: string | undefined,
): boolean {
  if (playerKey === undefined) return false;
  const entrant = view.players.find((p) => p.player_key === playerKey);
  return entrant !== undefined && !entrant.dropped;
}
