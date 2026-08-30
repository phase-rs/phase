const BYTE_UNITS = [
  { unit: "gigabyte", scale: 1_000_000_000 },
  { unit: "megabyte", scale: 1_000_000 },
] as const;

/**
 * A byte count as a person reads it, in the language the caller renders in.
 *
 * `Intl.NumberFormat`'s unit style rather than a hand-written suffix: six of
 * the app's seven locales use a comma decimal separator, and French writes
 * Go/Mo — a `toFixed(1) + " GB"` ships an English point and an English unit
 * into all six, in the panel whose entire subject is this number.
 *
 * Rendered on the decimal scale, which is what "GB"/"MB" mean beside a
 * browser's own quota figures. The engine's sampled size constants are
 * recorded in KiB, but they are six-sample medians and the 1000-vs-1024
 * difference is far inside their error; what must not happen is a label
 * implying one base while the arithmetic uses the other.
 *
 * It lives here rather than beside its first caller because two components in
 * the visual-pack panel now render byte figures, and exporting it from the
 * parent component would make the child import its own parent.
 */
export function formatByteSize(bytes: number, locale: string): string {
  const { unit, scale } = bytes >= BYTE_UNITS[0].scale ? BYTE_UNITS[0] : BYTE_UNITS[1];
  return new Intl.NumberFormat(locale, {
    style: "unit",
    unit,
    unitDisplay: "short",
    maximumFractionDigits: 1,
  }).format(bytes / scale);
}
