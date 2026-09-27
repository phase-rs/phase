interface CastableCardGlowProps {
  className?: string;
}

/**
 * A castability cue that lives entirely outside the printed card frame.
 * Keeping this as a separate, pointer-transparent layer preserves the card's
 * real border color while the soft cyan bloom supplies the Tabletop-style cue.
 */
export function CastableCardGlow({ className = "" }: CastableCardGlowProps) {
  return (
    <div
      aria-hidden
      data-castable-card-glow
      className={`pointer-events-none absolute -inset-px rounded-[4.4%/3.2%] ${className}`}
      style={{
        background: "rgba(34, 211, 238, 0.48)",
        boxShadow:
          "0 0 3px 1.5px rgba(103, 232, 249, 0.88), 0 0 6px 1.5px rgba(14, 165, 233, 0.42)",
        filter: "blur(1px)",
        opacity: 0.92,
      }}
    />
  );
}
