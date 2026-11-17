import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";

import type { CardType, ObjectId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { RichLabel } from "../mana/RichLabel.tsx";
import { CardTextboxPreview } from "./CardTextboxPreview.tsx";
import { DialogShell } from "./DialogShell.tsx";

export interface ChoiceOption {
  id: string;
  label: string;
  /** Use the compact explanatory treatment when the label is the option's full content. */
  labelTone?: "primary" | "secondary";
  description?: string;
  /** Optional glyph rendered before the label (e.g. a loyalty badge). */
  icon?: ReactNode;
  /**
   * Rendered as a non-selectable row (e.g. an ability whose cost the player
   * can't pay right now).
   *
   * Uses `aria-disabled`, NOT the native `disabled` attribute: `description`
   * renders as a `<p>` INSIDE the `<button>`, so a native `disabled` would drop
   * the explanatory reason out of the tab order and out of interactive
   * screen-reader navigation — exactly for the users who need it. The no-op
   * `onClick` guard is the real selection guard.
   */
  disabled?: boolean;
}

/**
 * How a `ChoiceModal` arranges its body.
 *
 * - `stacked`: the card preview, the options and the footer in one column.
 * - `columns`: for a short landscape screen, a phone on its side. The card
 *   preview sits in a left column; the options, side by side, sit above the
 *   footer in a right column. The dialog widens to hold both, and the prompt
 *   comes out far shorter than stacked. It needs a preview to fill the left
 *   column, and stacks without one.
 */
export type ChoiceModalLayout = "stacked" | "columns";

interface ChoiceModalProps {
  title: string;
  subtitle?: string;
  options: ChoiceOption[];
  onChoose: (id: string) => void;
  onClose?: () => void;
  /** Card name to preview above the options. Omit to skip the preview. */
  previewCardName?: string;
  /** Card type info for the preview. The preview uses this to pick the right
   * rules-text band for non-standard frames (saga, planeswalker, battle, etc.). */
  previewCardTypes?: CardType;
  /** When provided alongside previewCardName, hovering the inline preview fires
   * inspectObject for the floating full-card view. */
  previewObjectId?: ObjectId;
  footer?: ReactNode;
  /** Defaults to `stacked`. */
  layout?: ChoiceModalLayout;
}

export function ChoiceModal({
  title,
  subtitle,
  options,
  onChoose,
  onClose,
  previewCardName,
  previewCardTypes,
  previewObjectId,
  footer,
  layout = "stacked",
}: ChoiceModalProps) {
  const { t } = useTranslation("game");
  const ownerPrinting = useGameStore((s) =>
    previewObjectId == null ? undefined : s.gameState?.objects[previewObjectId]?.printing,
  );
  const ownerPrintedRef = useGameStore((s) =>
    previewObjectId == null ? undefined : s.gameState?.objects[previewObjectId]?.printed_ref,
  );

  const preview = previewCardName ? (
    <CardTextboxPreview
      cardName={previewCardName}
      cardTypes={previewCardTypes}
      ownerPrinting={ownerPrinting}
      ownerPrintedRef={ownerPrintedRef}
    />
  ) : null;
  const optionButtons = options.map((opt) => (
    <button
      key={opt.id}
      aria-disabled={opt.disabled || undefined}
      onClick={() => {
        if (!opt.disabled) onChoose(opt.id);
      }}
      className={
        opt.disabled
          ? "min-h-11 cursor-not-allowed rounded-[16px] border border-white/8 bg-white/[0.02] px-4 py-3 text-left opacity-60"
          : "min-h-11 rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/40"
      }
    >
      <span
        className={`flex items-center gap-2 ${opt.labelTone === "secondary" ? "text-xs font-normal text-slate-400" : "font-semibold text-white"}`}
      >
        {opt.icon && <span className="shrink-0">{opt.icon}</span>}
        <RichLabel text={opt.label} size={opt.labelTone === "secondary" ? "xs" : "sm"} />
      </span>
      {opt.description && (
        <p className="mt-1 text-xs text-slate-400">
          <RichLabel text={opt.description} size="xs" />
        </p>
      )}
    </button>
  ));
  const isColumns = layout === "columns" && preview != null;

  return (
    <DialogShell
      title={<RichLabel text={title} size="md" />}
      subtitle={subtitle ? <RichLabel text={subtitle} size="xs" /> : undefined}
      size={isColumns ? "lg" : "sm"}
      onClose={onClose}
      previewObjectId={previewObjectId}
    >
      {isColumns ? (
        <div className="grid grid-cols-2 gap-3 p-3">
          <div className="flex flex-col gap-1.5">
            {preview}
            {/* Holding the card opens the full-card preview, which DialogShell
                wires through `previewObjectId`. A mouse previews on hover
                instead, so only a coarse pointer gets the hint. */}
            {previewObjectId != null && (
              <p className="hidden text-center text-xs text-slate-400 pointer-coarse:block">
                {t("choiceModal.holdForFullCard")}
              </p>
            )}
          </div>
          <div className="flex flex-col gap-3">
            <div className="grid grid-cols-2 gap-2">{optionButtons}</div>
            {footer}
          </div>
        </div>
      ) : (
        <>
          {preview && <div className="px-3 pt-3 lg:px-5 lg:pt-4">{preview}</div>}
          <div className="px-3 py-3 lg:px-5 lg:py-5">
            <div className="flex flex-col gap-2">{optionButtons}</div>
            {footer && <div className="mt-3">{footer}</div>}
          </div>
        </>
      )}
    </DialogShell>
  );
}
