interface StatusBadgeProps {
  label: string;
  value?: number | string;
  tone?: "neutral" | "amber";
}

export function StatusBadge({
  label,
  value,
  tone = "neutral",
}: StatusBadgeProps) {
  return (
    <span
      className={`inline-flex items-center gap-1 rounded-full px-2 py-1 text-[10px] font-semibold tracking-[0.16em] uppercase ${
        tone === "amber"
          ? "bg-amber-400/16 text-amber-100 ring-1 ring-amber-300/30"
          : "bg-white/7 text-slate-200 ring-1 ring-white/10"
      }`}
    >
      <span>{label}</span>
      {value != null ? <span className="tabular-nums text-white">{value}</span> : null}
    </span>
  );
}

type CounterBadgeKind = "poison" | "speed";

interface CounterBadgeProps {
  kind: CounterBadgeKind;
  value: number;
}

export function CounterBadge({ kind, value }: CounterBadgeProps) {
  const isPoison = kind === "poison";
  const label = isPoison
    ? `${value} poison counter${value === 1 ? "" : "s"}`
    : `Speed ${value}`;
  const title = isPoison ? `Poison counters: ${value}` : `Speed: ${value}`;
  const urgent = isPoison && value >= 8;

  if (isPoison) {
    return (
      <span
        role="img"
        aria-label={label}
        title={title}
        className={`relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center overflow-hidden rounded-full px-1 text-[11px] font-black leading-none tabular-nums text-lime-950 ring-1 ${
          urgent
            ? "bg-lime-300 ring-lime-100 shadow-[0_0_16px_rgba(217,249,157,0.55)]"
            : "bg-lime-400 ring-lime-200/70 shadow-[0_0_12px_rgba(190,242,100,0.34)]"
        }`}
      >
        <span
          aria-hidden
          className="absolute inset-0 rounded-full bg-[radial-gradient(circle_at_30%_24%,rgba(255,255,255,0.9)_0_9%,transparent_11%),radial-gradient(circle_at_68%_30%,rgba(254,240,138,0.95)_0_7%,transparent_9%),radial-gradient(circle_at_38%_74%,rgba(132,204,22,0.72)_0_11%,transparent_13%),linear-gradient(135deg,#f7fee7_0%,#bef264_36%,#65a30d_72%,#1a2e05_100%)]"
        />
        <span
          aria-hidden
          className="absolute -bottom-1 left-1/2 h-3 w-5 -translate-x-1/2 rounded-[45%] bg-lime-950/28 blur-[1px]"
        />
        <span className="relative">{value}</span>
      </span>
    );
  }

  return (
    <span
      role="img"
      aria-label={label}
      title={title}
      className="relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center overflow-hidden rounded-[6px] px-1 text-[11px] font-black leading-none tabular-nums text-white ring-1 ring-slate-100/60 shadow-[0_0_10px_rgba(226,232,240,0.22)]"
    >
      <span
        aria-hidden
        className="absolute inset-0 bg-[linear-gradient(90deg,rgba(15,23,42,0.82)_0_2px,transparent_2px),linear-gradient(45deg,#f8fafc_25%,#020617_25%,#020617_50%,#f8fafc_50%,#f8fafc_75%,#020617_75%,#020617_100%)] bg-[length:100%_100%,7px_7px]"
      />
      <span aria-hidden className="absolute inset-0 bg-cyan-300/10" />
      <span className="relative rounded-sm bg-black/62 px-0.5">{value}</span>
    </span>
  );
}
