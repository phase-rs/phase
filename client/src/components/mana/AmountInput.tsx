import { useId } from "react";
import { useTranslation } from "react-i18next";

import { gameButtonClass } from "../ui/buttonStyles.ts";

/**
 * Shared bounded-amount control for the engine's amount prompts
 * (PayAmountChoice / ChooseXValue / AssistPayment).
 *
 * The `[min, max]` window is ENGINE-OWNED and arrives as props — this component holds no bound of
 * its own, no default, and no fallback. `parseAmount` is the single sanitization authority; it
 * REJECTS (returns null) rather than coercing, so a player never submits a number they did not type.
 */

/** Digit reading of `raw`, ignoring the window. Recovery uses this; SUBMISSION uses `parseAmount`. */
function digitsOf(raw: string): number | null {
  return /^\d+$/.test(raw) ? Number(raw) : null;
}

export function parseAmount(raw: string, min: number, max: number): number | null {
  // Digits only. `Number()` alone is NOT sufficient: MEASURED, Number("") === 0,
  // Number(" 7 ") === 7, Number("1.5") === 1.5, Number("1e3") === 1000, Number("+2") === 2 and
  // Number("0x10") === 16 all land INSIDE a typical window.
  const value = digitsOf(raw);
  return value !== null && value >= min && value <= max ? value : null;
}

export interface AmountInputLabels {
  /** aria-label for the numeric text box. */
  input: string;
  /** aria-label for the − stepper. */
  decrease: string;
  /** aria-label for the + stepper. */
  increase: string;
  /** Accessible name for the optional maximum shortcut. Providing this opts into the control. */
  setMaximum?: string;
}

export function AmountInput({
  raw,
  onRawChange,
  min,
  max,
  onSubmit,
  labels,
}: {
  raw: string;
  onRawChange: (raw: string) => void;
  min: number;
  max: number;
  /** Called on Enter. MUST itself reject an invalid amount — AmountInput deliberately does not
   *  re-guard, because a second guard would make the caller's guard unobservable and untestable. */
  onSubmit: () => void;
  labels: AmountInputLabels;
}) {
  const { t } = useTranslation("game");
  const amount = parseAmount(raw, min, max);
  const hintId = useId();
  const errorId = useId();

  // Recovery anchor. With the slider deleted the steppers are the only non-typing way out of an
  // invalid entry, so they stay LIVE while `amount === null` and snap back into [min, max]. They
  // step from the DIGIT reading, not from `amount`: `parseAmount` collapses "junk" and "out of
  // range" into the same null, so stepping from `amount ?? min` would throw away a perfectly
  // readable 1001 and jump to min. `parseAmount` gates SUBMISSION; `step` performs RECOVERY
  // toward the window.
  const step = (delta: number) =>
    onRawChange(String(Math.min(Math.max((digitsOf(raw) ?? min) + delta, min), max)));
  const decDisabled = amount !== null && amount <= min;
  const incDisabled = amount !== null && amount >= max;

  // ponytail: no showSlider/showSteppers flag — the slider is deleted, not configurable.
  // ponytail: no role="alert" — assertive per-keystroke announcements are the anti-pattern;
  //   aria-invalid + aria-describedby is the association.
  // ponytail: no pattern="[0-9]*" — inputMode="numeric" carries the modern-iOS keypad; add back
  //   only on a legacy-iOS report.
  // ponytail: the null-guard lives once, in the caller's handleCommit — a second guard in
  //   onKeyDown would make it unobservable.
  // role="spinbutton" IS carried, reversing an earlier note here that claimed no in-repo
  // precedent and unwanted aria-value* upkeep. Both premises were wrong: `ManaCurve.tsx` already
  // uses aria-valuenow, and the three controls this box replaced announced their value NATIVELY
  // (`type="range"` ⇒ role slider, `type="number"` ⇒ role spinbutton). Dropping to a bare
  // `type="text"` therefore made the ACCEPTED amount inaudible — pressing +/− or the arrow keys
  // mutated a value nothing exposed. The role is descriptive rather than decorative because the
  // box implements the pattern's CORE keyboard interaction (ArrowUp/ArrowDown step, clamped to
  // the window) — not the full APG list. Home/End (jump to min/max) are deliberately NOT
  // remapped: the host is a real editable text field where they carry load-bearing caret
  // semantics, and native `<input type="number">` — whose implicit role is already spinbutton —
  // does not remap them either.
  // `aria-valuenow` uses the VALIDATED amount, so it is absent while the entry is out of range
  // rather than contradicting aria-valuemin/max; `aria-invalid` carries that state instead.
  return (
    <div className="mb-4 px-2">
      <div className="flex items-center justify-center gap-2">
        <button
          type="button"
          onClick={() => step(-1)}
          disabled={decDisabled}
          aria-label={labels.decrease}
          className={gameButtonClass({
            tone: "neutral",
            size: "xs",
            disabled: decDisabled,
            // 44px REAL size, not a 36px box with an expanded `::before`. The pseudo-element
            // trick (as in `board/ManualManaToggle`) measured 42px here, not 44: `gameButtonClass`
            // adds `border`, and an absolutely positioned pseudo resolves against its ancestor's
            // PADDING box (36 − 2×1 = 34), so `-inset-1` yields 34 + 8. It works in
            // `ManualManaToggle` only because that control uses `ring-1`, which adds no layout
            // border. A size that must be derived to be checked is a size that will silently
            // regress; these steppers are the only non-typing recovery path out of an invalid
            // entry, so they get the boring, directly-readable 44px.
            // No `px-0`: `SIZE_CLASSES.xs` emits `px-2.5` later in the compiled sheet and wins,
            // so a `px-0` here would read as load-bearing while doing nothing. `w-11` pins the
            // border box at 44px regardless of padding (border-box sizing).
            className: "h-11 w-11 text-base",
          })}
        >
          −
        </button>
        <input
          type="text"
          inputMode="numeric"
          autoComplete="off"
          value={raw}
          onChange={(e) => onRawChange(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              onSubmit();
              return;
            }
            // type="text" has no native stepping; the accessibility floor requires arrows to step.
            if (e.key === "ArrowUp") {
              e.preventDefault();
              step(1);
            } else if (e.key === "ArrowDown") {
              e.preventDefault();
              step(-1);
            }
          }}
          aria-label={labels.input}
          role="spinbutton"
          aria-valuenow={amount ?? undefined}
          aria-valuemin={min}
          aria-valuemax={max}
          aria-invalid={amount === null}
          // The window is ANNOUNCED, not merely displayed. `type="range"` announced min/max/now
          // natively; a text box announces nothing, so the range hint is permanently associated
          // and the error message is appended to it while invalid.
          aria-describedby={amount === null ? `${hintId} ${errorId}` : hintId}
          // `w-24` not `w-20`: four digits must fit for the 1000 case. `h-11` (44px) because this
          // is the PRIMARY tap target of the control — fixing only the steppers would leave the
          // main one short. The `::before` idiom cannot be used here regardless: `<input>` is a
          // replaced element and renders no pseudo-elements.
          className={`h-11 w-24 rounded-lg border bg-gray-950/80 px-2 text-center font-mono text-base font-semibold shadow-inner outline-none transition focus:ring-2 ${
            amount === null
              ? "border-red-400/60 text-red-200 focus:ring-red-400/30"
              : "border-cyan-400/30 text-cyan-100 focus:ring-cyan-400/30"
          }`}
        />
        <button
          type="button"
          onClick={() => step(1)}
          disabled={incDisabled}
          aria-label={labels.increase}
          className={gameButtonClass({
            tone: "neutral",
            size: "xs",
            disabled: incDisabled,
            // 44px REAL size, not a 36px box with an expanded `::before`. The pseudo-element
            // trick (as in `board/ManualManaToggle`) measured 42px here, not 44: `gameButtonClass`
            // adds `border`, and an absolutely positioned pseudo resolves against its ancestor's
            // PADDING box (36 − 2×1 = 34), so `-inset-1` yields 34 + 8. It works in
            // `ManualManaToggle` only because that control uses `ring-1`, which adds no layout
            // border. A size that must be derived to be checked is a size that will silently
            // regress; these steppers are the only non-typing recovery path out of an invalid
            // entry, so they get the boring, directly-readable 44px.
            // No `px-0`: `SIZE_CLASSES.xs` emits `px-2.5` later in the compiled sheet and wins,
            // so a `px-0` here would read as load-bearing while doing nothing. `w-11` pins the
            // border box at 44px regardless of padding (border-box sizing).
            className: "h-11 w-11 text-base",
          })}
        >
          +
        </button>
        {labels.setMaximum && (
          <button
            type="button"
            onClick={() => onRawChange(String(max))}
            aria-label={labels.setMaximum}
            className={gameButtonClass({
              tone: "neutral",
              size: "xs",
              className: "h-11 text-base",
            })}
          >
            {t("mana.maximum")}
          </button>
        )}
        <span id={hintId} className="shrink-0 text-xs text-gray-500">
          {min > 0 ? t("mana.minMax", { min, max }) : t("mana.maxOnly", { max })}
        </span>
      </div>

      {/* PERMANENTLY MOUNTED, and a live region — both properties are load-bearing.
          `aria-invalid`/`aria-describedby` are resolved by a screen reader when focus ARRIVES at
          the box, but here they flip while focus is already inside it, so association alone
          announces nothing: the player would have to tab away and back to learn the entry was
          refused. Mounting the node INTO a live region is equally unreliable (regions announce
          MUTATIONS of existing content), so the region pre-exists and only its text changes.
          `role="status"` (polite) rather than "alert": the entry is being corrected mid-typing
          and must not interrupt what is already being read. `min-h-4` reserves the line so
          recovering from an invalid entry does not shift the panel under the pointer. */}
      <p
        id={errorId}
        role="status"
        aria-live="polite"
        className="mt-2 min-h-4 text-center text-xs text-red-300"
      >
        {amount === null ? t("mana.amountOutOfRange", { min, max }) : ""}
      </p>
    </div>
  );
}
