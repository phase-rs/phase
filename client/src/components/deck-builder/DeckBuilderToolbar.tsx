import { useTranslation } from "react-i18next";

import type { GameFormat } from "../../adapter/types";
import { FORMAT_REGISTRY } from "../../data/formatRegistry";
import { FormatFilter } from "./FormatFilter";
import { MenuSelect } from "../ui/MenuSelect";
import { SelectField } from "../ui/SelectField";

function PencilIcon({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 16 16" fill="currentColor" className={className} aria-hidden="true">
      <path d="M11.013 1.427a1.75 1.75 0 0 1 2.474 0l1.086 1.086a1.75 1.75 0 0 1 0 2.474l-8.61 8.61c-.21.21-.47.364-.756.445l-3.251.93a.75.75 0 0 1-.927-.928l.929-3.25c.081-.286.235-.547.445-.758l8.61-8.61Zm.176 4.823L9.75 4.81l-6.286 6.287a.253.253 0 0 0-.064.108l-.558 1.953 1.953-.558a.253.253 0 0 0 .108-.064L11.189 6.25Z" />
    </svg>
  );
}

interface DeckBuilderToolbarProps {
  onBack: () => void;
  deckName: string;
  onDeckNameChange: (name: string) => void;
  justSaved: boolean;
  onClearJustSaved: () => void;
  onSave: () => void;
  onClone: () => void;
  canClone: boolean;
  savedDecks: string[];
  onLoad: (name: string) => void;
  format: GameFormat;
  onFormatChange: (format: GameFormat) => void;
}

export function DeckBuilderToolbar({
  onBack,
  deckName,
  onDeckNameChange,
  justSaved,
  onClearJustSaved,
  onSave,
  onClone,
  canClone,
  savedDecks,
  onLoad,
  format,
  onFormatChange,
}: DeckBuilderToolbarProps) {
  const { t } = useTranslation("deck-builder");
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-2 border-b border-white/8 bg-black/18 px-3 py-2 backdrop-blur-md lg:px-4">
      <div className="flex min-w-0 flex-1 items-center gap-3 lg:flex-none">
        <button
          type="button"
          onClick={onBack}
          className="shrink-0 text-sm text-slate-400 hover:text-white"
        >
          &larr; {t("toolbar.menu")}
        </button>
        <div className="min-w-0 flex-1">
          <div className="text-[0.62rem] uppercase tracking-[0.22em] text-slate-500 lg:text-[0.68rem]">
            {t("toolbar.deckBuilder")}
          </div>
          {/* The title IS the name field — no separate read-only label + input.
              A trailing pencil signals it's tap-to-edit. */}
          <div className="group/name relative flex items-center">
            <input
              type="text"
              value={deckName}
              onChange={(e) => {
                onDeckNameChange(e.target.value);
                if (justSaved) onClearJustSaved();
              }}
              placeholder={t("toolbar.untitledDeck")}
              aria-label={t("toolbar.deckName")}
              className="w-full min-w-0 truncate rounded-md border border-transparent bg-transparent py-0.5 pl-1 pr-6 text-sm font-medium text-white placeholder-slate-500 hover:border-white/10 focus:border-white/20 focus:bg-black/18 focus:outline-none"
            />
            <PencilIcon className="pointer-events-none absolute right-1.5 h-3.5 w-3.5 text-slate-500 transition-colors group-focus-within/name:text-slate-300" />
          </div>
        </div>
      </div>

      <div className="order-3 flex w-full flex-col gap-2 lg:order-none lg:w-auto lg:flex-row lg:items-center">
        {/* Format: compact native select up to tablet, full button row at lg+
            (where there's horizontal room for the ~15-format wall). */}
        <SelectField
          value={format}
          onChange={(e) => onFormatChange(e.target.value as GameFormat)}
          aria-label={t("toolbar.format")}
          className="rounded-xl border border-white/10 bg-black/18 px-3 py-1.5 text-sm text-white focus:outline-none lg:hidden"
        >
          {FORMAT_REGISTRY.map(({ format: value, label }) => (
            <option key={value} value={value}>
              {label}
            </option>
          ))}
        </SelectField>
        <div className="hidden lg:block">
          <FormatFilter selected={format} onChange={onFormatChange} />
        </div>
      </div>

      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={onSave}
          disabled={!deckName.trim()}
          className={
            justSaved
              ? "shrink-0 rounded-xl border border-emerald-400/40 bg-emerald-500/20 px-3 py-1.5 text-sm text-emerald-200 disabled:opacity-40"
              : "shrink-0 rounded-xl border border-white/10 bg-white/10 px-3 py-1.5 text-sm text-white hover:bg-white/14 disabled:opacity-40"
          }
        >
          {justSaved ? t("toolbar.saved") : t("common:actions.save")}
        </button>
        <button
          type="button"
          onClick={onClone}
          disabled={!canClone}
          title={t("toolbar.cloneTitle")}
          className="shrink-0 rounded-xl border border-white/10 bg-black/18 px-3 py-1.5 text-sm text-slate-200 hover:bg-white/6 disabled:opacity-40"
        >
          {t("toolbar.clone")}
        </button>
        {savedDecks.length > 0 && (
          <MenuSelect
            label={t("toolbar.loadDeck")}
            items={savedDecks.map((name) => ({ value: name, label: name }))}
            onSelect={onLoad}
            wrapperClassName="shrink-0"
          />
        )}
      </div>
    </div>
  );
}
