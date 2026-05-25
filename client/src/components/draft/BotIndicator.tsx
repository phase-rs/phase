interface BotIndicatorProps {
  label: string;
  size?: "sm" | "md";
}

export function BotIndicator({ label, size = "md" }: BotIndicatorProps) {
  const boxSize = size === "sm" ? "h-3.5 w-3.5" : "h-4 w-4";
  return (
    <span
      aria-label={label}
      title={label}
      className={`relative inline-flex ${boxSize} shrink-0 items-center justify-center rounded-[4px] border border-cyan-300/35 bg-cyan-300/12`}
    >
      <span className="absolute -top-1 h-1 w-px bg-cyan-200/70" />
      <span className="flex gap-0.5">
        <span className="h-0.5 w-0.5 rounded-full bg-cyan-100" />
        <span className="h-0.5 w-0.5 rounded-full bg-cyan-100" />
      </span>
    </span>
  );
}
