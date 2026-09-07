import { useId, useState } from "react";
import { useTranslation } from "react-i18next";

import type { BracketShape, GameFormat, MatchArity, ScoringPolicy } from "../../adapter/types";
import type { CreateTournamentRequest } from "../../services/tournamentClient";
import { FORMAT_REGISTRY } from "../../data/formatRegistry";
import { defaultScoringForArity } from "../../pages/tournamentPageState";

interface CreateTournamentFormProps {
  onSubmit: (req: CreateTournamentRequest) => void;
  submitting?: boolean;
  /** Arity the form opens on. Defaults to head-to-head. */
  initialArity?: MatchArity;
}

/**
 * Parses a numeric field, keeping the previous value when the field is not a
 * number. Deliberately does not clamp or validate a range — `MatchArity::new`
 * (`crates/lobby-broker/src/tournament.rs:96-113`) and `ScoringPolicy::new`
 * are the broker's, and duplicating their bounds here would be a second,
 * drifting copy of a rule the server already owns.
 */
function parsedOr(raw: string, fallback: number): number {
  const parsed = Number.parseInt(raw, 10);
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
  /** Empty string means "Automatic" — the wire's `total_rounds: null`. */
  const [roundsInput, setRoundsInput] = useState("");
  /**
   * "Automatic + N": extra rounds added on top of the auto-derived count. Only
   * meaningful while `roundsInput` is empty (Automatic) — an exact count and a
   * plus-N addend are mutually exclusive, which the broker also enforces, so
   * this is dropped rather than sent alongside an explicit round count.
   */
  const [plusRoundsInput, setPlusRoundsInput] = useState("");
  const [scoring, setScoring] = useState<ScoringPolicy>(() =>
    defaultScoringForArity(initialArity),
  );
  /**
   * Latches the moment the organizer edits any scoring field. Until then the
   * prefill follows the arity (2 -> 3/1/0, 4 -> 7/1/0); afterwards the
   * organizer's values are authoritative and survive an arity change.
   */
  const [scoringTouched, setScoringTouched] = useState(false);

  function changeArity(next: MatchArity) {
    setArity(next);
    if (!scoringTouched) setScoring(defaultScoringForArity(next));
  }

  function changeScoring(patch: Partial<ScoringPolicy>) {
    setScoringTouched(true);
    setScoring((current) => ({ ...current, ...patch }));
  }

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
          scoring,
          bracket,
          totalRounds,
          plusRounds,
          // `""` is the "no format named" choice; everything else is a
          // `GameFormat` submitted verbatim.
          format: format === "" ? null : format,
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
          onChange={(event) => changeArity(parsedOr(event.target.value, arity))}
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
        <div className="flex gap-3">
          <div className="flex flex-1 flex-col gap-1">
            <label htmlFor={winId} className="text-xs text-gray-500">
              {t("create.winPointsLabel")}
            </label>
            <input
              id={winId}
              type="number"
              value={scoring.win_points}
              onChange={(event) =>
                changeScoring({
                  win_points: parsedOr(event.target.value, scoring.win_points),
                })
              }
              className="rounded-[6px] border border-white/10 bg-black/30 px-3 py-2 text-sm text-gray-100"
            />
          </div>
          <div className="flex flex-1 flex-col gap-1">
            <label htmlFor={drawId} className="text-xs text-gray-500">
              {t("create.drawPointsLabel")}
            </label>
            <input
              id={drawId}
              type="number"
              value={scoring.draw_points}
              onChange={(event) =>
                changeScoring({
                  draw_points: parsedOr(event.target.value, scoring.draw_points),
                })
              }
              className="rounded-[6px] border border-white/10 bg-black/30 px-3 py-2 text-sm text-gray-100"
            />
          </div>
          <div className="flex flex-1 flex-col gap-1">
            <label htmlFor={lossId} className="text-xs text-gray-500">
              {t("create.lossPointsLabel")}
            </label>
            <input
              id={lossId}
              type="number"
              value={scoring.loss_points}
              onChange={(event) =>
                changeScoring({
                  loss_points: parsedOr(event.target.value, scoring.loss_points),
                })
              }
              className="rounded-[6px] border border-white/10 bg-black/30 px-3 py-2 text-sm text-gray-100"
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
