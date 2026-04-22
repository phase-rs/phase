import { ManaSymbol } from "./ManaSymbol.tsx";

interface RichLabelProps {
  text: string;
  size?: "xs" | "sm" | "md" | "lg";
  className?: string;
}

const SYMBOL_PATTERN = /\{([^{}]+)\}/g;

export function RichLabel({ text, size = "sm", className }: RichLabelProps) {
  return (
    <span className={className}>
      {/* ChoiceModal uses brace-delimited mana/tap notation like {W} and {T}. */}
      {text.split(SYMBOL_PATTERN).map((part, i) =>
        i % 2 === 0 ? (
          part
        ) : (
          <ManaSymbol key={i} shard={part} size={size} className="align-[-0.125em]" />
        ),
      )}
    </span>
  );
}
