interface LegalityBadgeProps {
  legalities: Record<string, string>;
  format: string;
}

const STATUS_STYLES: Record<string, string> = {
  legal: "bg-green-700/60 text-green-300",
  banned: "bg-red-700/60 text-red-300",
  restricted: "bg-yellow-700/60 text-yellow-300",
  not_legal: "bg-gray-700/60 text-gray-400",
};

const STATUS_LABELS: Record<string, string> = {
  legal: "Legal",
  banned: "Banned",
  restricted: "Restricted",
  not_legal: "Not Legal",
};

export function LegalityBadge({ legalities, format }: LegalityBadgeProps) {
  const status = (legalities[format] ?? "not_legal").toLowerCase();
  const style = STATUS_STYLES[status] ?? STATUS_STYLES.not_legal;
  const label = STATUS_LABELS[status] ?? "Not Legal";

  return (
    <span
      className={`inline-block rounded px-1.5 py-0.5 text-[9px] font-semibold leading-tight ${style}`}
    >
      {label}
    </span>
  );
}
