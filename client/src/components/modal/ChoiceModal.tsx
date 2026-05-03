import type { CardType } from "../../adapter/types.ts";
import { RichLabel } from "../mana/RichLabel.tsx";
import { CardTextboxPreview } from "./CardTextboxPreview.tsx";
import { DialogShell } from "./DialogShell.tsx";

export interface ChoiceOption {
  id: string;
  label: string;
  description?: string;
}

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
}

export function ChoiceModal({
  title,
  subtitle,
  options,
  onChoose,
  onClose,
  previewCardName,
  previewCardTypes,
}: ChoiceModalProps) {
  return (
    <DialogShell
      title={<RichLabel text={title} size="md" />}
      subtitle={subtitle ? <RichLabel text={subtitle} size="xs" /> : undefined}
      onClose={onClose}
    >
      {previewCardName && (
        <div className="px-3 pt-3 lg:px-5 lg:pt-4">
          <CardTextboxPreview
            cardName={previewCardName}
            cardTypes={previewCardTypes}
          />
        </div>
      )}
      <div className="px-3 py-3 lg:px-5 lg:py-5">
        <div className="flex flex-col gap-2">
          {options.map((opt) => (
            <button
              key={opt.id}
              onClick={() => onChoose(opt.id)}
              className="min-h-11 rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/40"
            >
              <span className="font-semibold text-white">
                <RichLabel text={opt.label} size="sm" />
              </span>
              {opt.description && (
                <p className="mt-1 text-xs text-slate-400">
                  <RichLabel text={opt.description} size="xs" />
                </p>
              )}
            </button>
          ))}
        </div>
      </div>
    </DialogShell>
  );
}
