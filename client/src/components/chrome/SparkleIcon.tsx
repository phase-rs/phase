/** Sparkle glyph for the "What's New" affordance, shared by the desktop rail
 * and the mobile tab bar so both stay visually identical. */
export function SparkleIcon({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true" className={className}>
      <path d="M12 2l1.9 5.1L19 9l-5.1 1.9L12 16l-1.9-5.1L5 9l5.1-1.9L12 2zm6 11l.95 2.55L21.5 16.5l-2.55.95L18 20l-.95-2.55L14.5 16.5l2.55-.95L18 13zM6 14l.7 1.9L8.6 16.6l-1.9.7L6 19.2l-.7-1.9L3.4 16.6l1.9-.7L6 14z" />
    </svg>
  );
}
