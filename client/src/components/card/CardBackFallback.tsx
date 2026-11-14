import type { CSSProperties } from "react";
import { useTranslation } from "react-i18next";

import { useCardBackImage } from "../../hooks/useCardImage.ts";

interface CardBackFallbackProps {
  className?: string;
  style?: CSSProperties;
}

/** A fixed public card back. Its API deliberately cannot carry face identity. */
export function CardBackFallback({ className = "", style }: CardBackFallbackProps) {
  const { t } = useTranslation("game");
  const { src, advanceFailedSource } = useCardBackImage();
  const label = t("hand.cardBack");

  if (src) {
    return (
      <img
        src={src}
        alt={label}
        draggable={false}
        className={`object-cover ${className}`}
        style={style}
        onError={() => advanceFailedSource?.(src)}
      />
    );
  }

  return (
    <div
      role="img"
      aria-label={label}
      className={`aspect-[488/680] overflow-hidden bg-[#17191d] p-[6%] ${className}`}
      style={style}
    >
      <div className="h-full w-full rounded-[8%] border border-slate-500/40 bg-[radial-gradient(circle_at_center,#344055_0%,#1e293b_38%,#080b12_75%)] shadow-[inset_0_0_0_2px_rgba(255,255,255,0.04)]" />
    </div>
  );
}
