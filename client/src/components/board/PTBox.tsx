import type { PTDisplay, PTColor } from "../../viewmodel/cardProps";

interface PTBoxProps {
  ptDisplay: PTDisplay;
}

const COLOR_CLASSES: Record<PTColor, string> = {
  green: "text-green-400",
  red: "text-red-400",
  white: "text-white",
};

export function PTBox({ ptDisplay }: PTBoxProps) {
  return (
    <div className="absolute bottom-0 right-0 z-20 flex items-center gap-px rounded-tl bg-black/80 px-1.5 py-0.5 text-xs font-bold">
      <span className={COLOR_CLASSES[ptDisplay.powerColor]}>
        {ptDisplay.power}
      </span>
      <span className="text-gray-400">/</span>
      <span className={COLOR_CLASSES[ptDisplay.toughnessColor]}>
        {ptDisplay.toughness}
      </span>
    </div>
  );
}
