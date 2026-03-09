interface ManaSymbolProps {
  shard: string;
  size?: "sm" | "md" | "lg";
  className?: string;
}

const SIZE_CLASSES = {
  sm: "w-5 h-5 text-[10px]",
  md: "w-6 h-6 text-xs",
  lg: "w-8 h-8 text-sm",
} as const;

const COLOR_MAP: Record<string, string> = {
  W: "bg-yellow-200 text-yellow-900",
  U: "bg-blue-500 text-white",
  B: "bg-gray-800 text-gray-200 ring-1 ring-gray-600",
  R: "bg-red-500 text-white",
  G: "bg-green-600 text-white",
  C: "bg-gray-400 text-gray-900",
  X: "bg-gray-500 text-white",
};

/** CSS background colors for gradient splits */
const GRADIENT_COLORS: Record<string, string> = {
  W: "#fef08a", // yellow-200
  U: "#3b82f6", // blue-500
  B: "#1f2937", // gray-800
  R: "#ef4444", // red-500
  G: "#16a34a", // green-600
  C: "#9ca3af", // gray-400
};

function isHybrid(shard: string): boolean {
  return shard.includes("/") && !shard.endsWith("/P");
}

function isPhyrexian(shard: string): boolean {
  return shard.endsWith("/P");
}

function isGenericNumber(shard: string): boolean {
  return /^\d+$/.test(shard);
}

export function ManaSymbol({
  shard,
  size = "md",
  className = "",
}: ManaSymbolProps) {
  const base = `inline-flex items-center justify-center rounded-full font-bold ${SIZE_CLASSES[size]}`;

  if (isHybrid(shard)) {
    const [a, b] = shard.split("/");
    const colorA = GRADIENT_COLORS[a] ?? "#6b7280";
    const colorB = GRADIENT_COLORS[b] ?? "#6b7280";
    return (
      <span
        className={`${base} text-white ${className}`}
        style={{
          background: `linear-gradient(to bottom, ${colorA} 50%, ${colorB} 50%)`,
        }}
        title={shard}
      >
        <span className="drop-shadow-[0_1px_1px_rgba(0,0,0,0.6)]">
          {a}/{b}
        </span>
      </span>
    );
  }

  if (isPhyrexian(shard)) {
    const color = shard.split("/")[0];
    const colorClass = COLOR_MAP[color] ?? "bg-gray-500 text-white";
    return (
      <span className={`relative ${base} ${colorClass} ${className}`} title={shard}>
        {color}
        <span className="absolute -bottom-0.5 -right-0.5 text-[8px] leading-none opacity-80">
          P
        </span>
      </span>
    );
  }

  if (isGenericNumber(shard)) {
    return (
      <span
        className={`${base} bg-gray-500 text-white ${className}`}
        title={`Generic ${shard}`}
      >
        {shard}
      </span>
    );
  }

  const colorClass = COLOR_MAP[shard] ?? "bg-gray-500 text-white";
  return (
    <span className={`${base} ${colorClass} ${className}`} title={shard}>
      {shard}
    </span>
  );
}
