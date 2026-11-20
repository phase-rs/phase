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
 *   column, and stacks without one. While the card's art loads, the left
 *   column keeps its place (holding the hint on a touch screen), so the
 *   prompt does not collapse to stacked and back when the art arrives.
 *   Its height budget assumes the standard rules-text band; a taller band (a
 *   Saga's) makes the card column taller.
 */
export type ChoiceModalLayout = "stacked" | "columns";

/**
 * Each layout's dialog width and body classes. Both layouts share one element
 * tree, so a phone that turns with a prompt open keeps the prompt's nodes, and
 * with them focus and the preview's loaded image.
 */
const LAYOUT_STYLES = {
  stacked: {
    dialogSize: "sm",
    body: "",
    cardColumn: "px-3 pt-3 lg:px-5 lg:pt-4",
    actionColumn: "px-3 py-3 lg:px-5 lg:py-5",
    options: "flex flex-col gap-2",
    footer: "mt-3",
  },
  columns: {
    dialogSize: "lg",
    body: "grid grid-cols-2 gap-3 p-3 lg:px-5",
    cardColumn: "flex flex-col gap-1.5",
    actionColumn: "flex flex-col gap-3",
    options: "grid grid-cols-2 gap-2",
    footer: "",
  },
} as const satisfies Record<
  ChoiceModalLayout,
  {
    dialogSize: "sm" | "lg";
    body: string;
    cardColumn: string;
    actionColumn: string;
    options: string;
    footer: string;
  }
>;

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
  const resolvedLayout: ChoiceModalLayout =
    layout === "columns" && preview != null ? "columns" : "stacked";
  const styles = LAYOUT_STYLES[resolvedLayout];

  return (
    <DialogShell
      title={<RichLabel text={title} size="md" />}
      subtitle={subtitle ? <RichLabel text={subtitle} size="xs" /> : undefined}
      size={styles.dialogSize}
      onClose={onClose}
      previewObjectId={previewObjectId}
    >
      <div className={styles.body}>
        {preview && (
          <div className={styles.cardColumn}>
            {preview}
            {/* Holding the card opens the full-card preview, which DialogShell
                wires through `previewObjectId`. A mouse previews on hover
                instead, so only a coarse pointer gets the hint. */}
            {resolvedLayout === "columns" && previewObjectId != null && (
              <p className="hidden text-center text-xs text-slate-400 pointer-coarse:block">
                {t("choiceModal.holdForFullCard")}
              </p>
            )}
          </div>
        )}
        <div className={styles.actionColumn}>
          <div className={styles.options}>{optionButtons}</div>
          {footer && <div className={styles.footer}>{footer}</div>}
        </div>
      </div>
    </DialogShell>
  );
}
