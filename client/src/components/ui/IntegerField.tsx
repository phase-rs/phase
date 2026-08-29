import { useState } from "react";

/**
 * Bounded integer text box for setup forms.
 *
 * Exists because the obvious controlled spelling —
 * `value={n} onChange={e => set(Math.max(min, parseInt(e.target.value) || fallback))}` —
 * silently corrupts what the user typed. Clearing the box makes `parseInt("")`
 * NaN, the fallback re-renders the box with that number still in it, and the
 * digits typed next append to it: clear a "40" and type "25" and the value
 * committed is 125. The per-keystroke clamp that was meant to keep NaN out of
 * the engine is what mangles the entry.
 *
 * So the raw text is held here and the caller is only told about readings that
 * actually parse — the same reject-don't-coerce rule `AmountInput` follows for
 * the engine's in-game amount prompts. An empty or half-typed box leaves the
 * last committed value standing, and blurring re-syncs the display to it, so
 * `value` is always a number the user really entered.
 */
export function IntegerField({
  id,
  value,
  min,
  onCommit,
  className,
  ariaLabel,
}: {
  id?: string;
  /** The committed value. Shown whenever the box is not being edited. */
  value: number;
  min: number;
  /** Called only for a reading that parses to an integer at or above `min`. */
  onCommit: (next: number) => void;
  className?: string;
  ariaLabel?: string;
}) {
  // `null` = not editing, show `value`. A string = the user's in-progress text,
  // which may legitimately be empty or below `min` mid-typing.
  const [draft, setDraft] = useState<string | null>(null);

  return (
    <input
      id={id}
      type="number"
      min={min}
      aria-label={ariaLabel}
      value={draft ?? String(value)}
      onChange={(e) => {
        const raw = e.target.value;
        setDraft(raw);
        const parsed = Number.parseInt(raw, 10);
        if (Number.isFinite(parsed) && parsed >= min) {
          onCommit(parsed);
        }
      }}
      onBlur={() => setDraft(null)}
      className={className}
    />
  );
}
