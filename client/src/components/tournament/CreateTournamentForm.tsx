import { useId, useState } from "react";
import { useTranslation } from "react-i18next";

import type {
  BracketShape,
  GameFormat,
  MatchArity,
  MatchType,
} from "../../adapter/types";
import type { CreateTournamentRequest } from "../../services/tournamentClient";
import { FORMAT_REGISTRY } from "../../data/formatRegistry";

interface CreateTournamentFormProps {
  onSubmit: (req: CreateTournamentRequest) => void;
  submitting?: boolean;
  /** Arity the form opens on. Defaults to head-to-head. */
  initialArity?: MatchArity;
}

/**
 * Parses a numeric field, keeping `fallback` when the field is blank or not a
 * number. Uses `Number` rather than `parseInt` so the COMPLETE value is read:
 * a `type="number"` input accepts exponent notation (`1e2`) and fractions, both
 * of which `parseInt` would silently truncate (`1e2` -> `1`) before the value
 * reaches the broker. Deliberately does not clamp or validate a range —
 * `MatchArity::new` (`crates/lobby-broker/src/tournament.rs:96-113`) and
 * `ScoringPolicy::new` are the broker's, and duplicating their bounds here would
 * be a second, drifting copy of a rule the server already owns.
 */
function parsedOr(raw: string, fallback: number): number {
  if (raw.trim() === "") return fallback;
  const parsed = Number(raw);
  return Number.isNaN(parsed) ? fallback : parsed;
}

export function CreateTournamentForm({
  onSubmit,
  submitting = false,
  initialArity = 2,
}: CreateTournamentFormProps) {
  const { t } = useTranslation("tournament");
  const nameId = useId();
  const arityId = useId();
  const arityHintId = useId();
  const bracketId = useId();
  const formatId = useId();
  const matchTypeId = useId();
  const roundsId = useId();
  const plusRoundsId = useId();
  const plusRoundsHintId = useId();
  const winId = useId();
  const drawId = useId();
  const lossId = useId();

  const [name, setName] = useState("");
  const [arity, setArity] = useState<MatchArity>(initialArity);
  const [bracket, setBracket] = useState<BracketShape>("Swiss");
  /** Empty string means "no format named" — the wire's `format: null`. */
  const [format, setFormat] = useState<GameFormat | "">("");
  /**
   * The head-to-head match structure. Best-of-three is inherently 2-player, so a
   * pod (arity !== 2) always submits Bo1 regardless of this control (which is
   * disabled there). See `podForcesBo1`.
   */
  const [matchType, setMatchType] = useState<MatchType>("Bo3");
  /** Empty string means "Automatic" — the wire's `total_rounds: null`. */
  const [roundsInput, setRoundsInput] = useState("");
  /**
   * "Automatic + N": extra rounds added on top of the auto-derived count. Only
   * meaningful while `roundsInput` is empty (Automatic) — an exact count and a
   * plus-N addend are mutually exclusive, which the broker also enforces, so
   * this is dropped rather than sent alongside an explicit round count.
   */
  const [plusRoundsInput, setPlusRoundsInput] = useState("");
  /**
   * The match-point axis, as an "Automatic" toggle over an explicit override.
   *
   * `automaticScoring` (default on) submits `scoring: null`: as of lobby
   * protocol v6 the broker owns the default (`ScoringPolicy::default_for_arity`)
   * and sends the resolved value back on `TournamentSummary.scoring`. This form
   * computes no default at all — that duplicate is what the wire field exists to
   * delete.
   *
   * The three override fields are STRING-backed and parsed at submit, exactly
   * like the rounds field above — deliberately NOT numeric `value` +
   * `parsedOr(current)` state, whose "an emptied field reverts to its current
   * value" behaviour made a draw of 1 impossible to clear and retype as 2.
   */
  const [automaticScoring, setAutomaticScoring] = useState(true);
  const [winInput, setWinInput] = useState("");
  const [drawInput, setDrawInput] = useState("");
  const [lossInput, setLossInput] = useState("");

  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        // Submit exactly what was chosen. No legality check of any kind lives
        // here — notably `SingleElimination` with an arity other than 2, which
        // the broker refuses at `crates/lobby-broker/src/tournament.rs:1514-1523`,
        // and an explicit round count of 0, refused at `:1524`. Both must reach
        // the wire so the server stays the single authority.
        const parsedRounds = Number.parseInt(roundsInput, 10);
        const totalRounds = Number.isNaN(parsedRounds) ? null : parsedRounds;
        // The plus-N addend is only sent when the round count is Automatic —
        // an exact `total_rounds` wins outright and the two are mutually
        // exclusive (the broker rejects both), so never put them on the wire
        // together.
        const parsedPlus = Number.parseInt(plusRoundsInput, 10);
        const plusRounds =
          totalRounds === null && !Number.isNaN(parsedPlus) ? parsedPlus : null;
        onSubmit({
          name,
          arity,
          // `null` when Automatic: the broker applies its arity default and
          // returns the resolved policy on the summary. An explicit override is
          // parsed from the string inputs (empty -> 0) and submitted verbatim,
          // unvalidated, exactly like every other field.
          scoring: automaticScoring
            ? null
            : {
                win_points: parsedOr(winInput, 0),
                draw_points: parsedOr(drawInput, 0),
                loss_points: parsedOr(lossInput, 0),
              },
          bracket,
          totalRounds,
          plusRounds,
          // `""` is the "no format named" choice; everything else is a
          // `GameFormat` submitted verbatim.
          format: format === "" ? null : format,
          // Bo3 is inherently 2-player. For a pod we send `null` and let the
          // broker resolve the arity default (single-game per MSTR) rather than
          // duplicate that rule here; the disabled selector below is a UI
          // affordance only. Head-to-head sends the organizer's explicit choice.
          matchType: arity === 2 ? matchType : null,
        });
      }}
      className="flex flex-col gap-4 rounded-xl border border-white/10 bg-black/20 p-4"
    >
      <h2 className="text-lg font-semibold text-gray-100">{t("create.heading")}</h2>

      <div className="flex flex-col gap-1">
        <label htmlFor={nameId} className="text-xs text-gray-400">
          {t("create.nameLabel")}
        </label>
        <input
          id={nameId}
          type="text"
          value={name}
          placeholder={t("create.namePlaceholder")}
          onChange={(event) => setName(event.target.value)}
          className="rounded-[6px] border border-white/10 bg-black/30 px-3 py-2 text-sm text-gray-100"
        />
      </div>

      <div className="flex flex-col gap-1">
        <label htmlFor={arityId} className="text-xs text-gray-400">
          {t("create.arityLabel")}
        </label>
        <input
          id={arityId}
          type="number"
          value={arity}
          aria-describedby={arityHintId}
          onChange={(event) => setArity(parsedOr(event.target.value, arity))}
          className="rounded-[6px] border border-white/10 bg-black/30 px-3 py-2 text-sm text-gray-100"
        />
        <p id={arityHintId} className="text-xs text-gray-500">
          {t("create.arityHint")}
        </p>
      </div>

      <div className="flex flex-col gap-1">
        <label htmlFor={bracketId} className="text-xs text-gray-400">
          {t("create.bracketLabel")}
        </label>
        <select
          id={bracketId}
          value={bracket}
          onChange={(event) => setBracket(event.target.value as BracketShape)}
          className="rounded-[6px] border border-white/10 bg-black/30 px-3 py-2 text-sm text-gray-100"
        >
          <option value="Swiss">{t("bracket.Swiss")}</option>
          <option value="SingleElimination">{t("bracket.SingleElimination")}</option>
        </select>
      </div>

      <div className="flex flex-col gap-1">
        <label htmlFor={formatId} className="text-xs text-gray-400">
          {t("create.formatLabel")}
        </label>
        {/* A display label only — the tournament enforces no deck legality.
            Options come from the shared FORMAT_REGISTRY (the same list the game
            host format picker renders), so a new engine format appears here
            with no change to this component. */}
        <select
          id={formatId}
          value={format}
          onChange={(event) => setFormat(event.target.value as GameFormat | "")}
          className="rounded-[6px] border border-white/10 bg-black/30 px-3 py-2 text-sm text-gray-100"
        >
          <option value="">{t("create.formatNone")}</option>
          {FORMAT_REGISTRY.map((meta) => (
            <option key={meta.format} value={meta.format}>
              {meta.label}
            </option>
          ))}
        </select>
      </div>

      <div className="flex flex-col gap-1">
        <label htmlFor={matchTypeId} className="text-xs text-gray-400">
          {t("create.matchTypeLabel")}
        </label>
        {/* Best-of-three is inherently 2-player; a pod is always single-game
            (MSTR), which the broker enforces — so the control is disabled and
            reads Bo1 at any arity other than head-to-head. */}
        <select
          id={matchTypeId}
          value={arity === 2 ? matchType : "Bo1"}
          disabled={arity !== 2}
          onChange={(event) => setMatchType(event.target.value as MatchType)}
          className="rounded-[6px] border border-white/10 bg-black/30 px-3 py-2 text-sm text-gray-100 disabled:opacity-50"
        >
          <option value="Bo3">{t("create.matchTypeBo3")}</option>
          <option value="Bo1">{t("create.matchTypeBo1")}</option>
        </select>
      </div>

      <div className="flex flex-col gap-1">
        <label htmlFor={roundsId} className="text-xs text-gray-400">
          {t("create.totalRoundsLabel")}
        </label>
        {/* Empty is the "Automatic" affordance: `total_rounds` is the one
            `CreateTournament` field the wire defaults (`protocol.rs:697-698`),
            so an omitted value is expressible and is submitted as `null`. */}
        <input
          id={roundsId}
          type="number"
          value={roundsInput}
          placeholder={t("create.totalRoundsAuto")}
          onChange={(event) => setRoundsInput(event.target.value)}
          className="rounded-[6px] border border-white/10 bg-black/30 px-3 py-2 text-sm text-gray-100"
        />
      </div>

      <div className="flex flex-col gap-1">
        <label htmlFor={plusRoundsId} className="text-xs text-gray-400">
          {t("create.plusRoundsLabel")}
        </label>
        {/* "Swiss plus N": extra rounds added on top of the automatic count.
            Applies only while the round count above is left Automatic; an
            explicit count and a plus-N addend are mutually exclusive. */}
        <input
          id={plusRoundsId}
          type="number"
          value={plusRoundsInput}
          aria-describedby={plusRoundsHintId}
          onChange={(event) => setPlusRoundsInput(event.target.value)}
          className="rounded-[6px] border border-white/10 bg-black/30 px-3 py-2 text-sm text-gray-100"
        />
        <p id={plusRoundsHintId} className="text-xs text-gray-500">
          {t("create.plusRoundsHint")}
        </p>
      </div>

      <fieldset className="flex flex-col gap-2">
        <legend className="text-xs text-gray-400">{t("create.scoringLabel")}</legend>
        {/* The "Automatic" affordance, mirroring the rounds field above: checked
            (default) submits `scoring: null` and the broker applies its arity
            default; unchecking reveals the three inputs as an explicit override.
            Reuses the existing `create.totalRoundsAuto` label rather than
            minting a new catalog key across all locales. */}
        <label className="flex items-center gap-2 text-xs text-gray-500">
          <input
            type="checkbox"
            checked={automaticScoring}
            onChange={(event) => setAutomaticScoring(event.target.checked)}
          />
          {t("create.totalRoundsAuto")}
        </label>
        <div className="flex gap-3">
          <div className="flex flex-1 flex-col gap-1">
            <label htmlFor={winId} className="text-xs text-gray-500">
              {t("create.winPointsLabel")}
            </label>
            <input
              id={winId}
              type="number"
              disabled={automaticScoring}
              value={winInput}
              placeholder={t("create.totalRoundsAuto")}
              onChange={(event) => setWinInput(event.target.value)}
              className="rounded-[6px] border border-white/10 bg-black/30 px-3 py-2 text-sm text-gray-100 disabled:opacity-50"
            />
          </div>
          <div className="flex flex-1 flex-col gap-1">
            <label htmlFor={drawId} className="text-xs text-gray-500">
              {t("create.drawPointsLabel")}
            </label>
            <input
              id={drawId}
              type="number"
              disabled={automaticScoring}
              value={drawInput}
              placeholder={t("create.totalRoundsAuto")}
              onChange={(event) => setDrawInput(event.target.value)}
              className="rounded-[6px] border border-white/10 bg-black/30 px-3 py-2 text-sm text-gray-100 disabled:opacity-50"
            />
          </div>
          <div className="flex flex-1 flex-col gap-1">
            <label htmlFor={lossId} className="text-xs text-gray-500">
              {t("create.lossPointsLabel")}
            </label>
            <input
              id={lossId}
              type="number"
              disabled={automaticScoring}
              value={lossInput}
              placeholder={t("create.totalRoundsAuto")}
              onChange={(event) => setLossInput(event.target.value)}
              className="rounded-[6px] border border-white/10 bg-black/30 px-3 py-2 text-sm text-gray-100 disabled:opacity-50"
            />
          </div>
        </div>
      </fieldset>

      <button
        type="submit"
        disabled={submitting}
        className="rounded-[6px] bg-emerald-600 px-4 py-2 text-sm font-semibold text-white disabled:bg-gray-700 disabled:text-gray-500"
      >
        {submitting ? t("create.submitting") : t("create.submit")}
      </button>
    </form>
  );
}
