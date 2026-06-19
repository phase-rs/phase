/**
 * Shared cube setup panel — paste a counted cube list, configure pack/seat
 * settings, and dispatch via the `onStart` callback. Decoupled from any
 * specific draft store so both solo (DraftPage) and multiplayer
 * (DraftPodPage) flows can mount it.
 */

import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { MenuSelect } from "../ui/MenuSelect";

import type { CubeDraftSettings } from "../../adapter/draft-adapter";
import { menuButtonClass } from "../menu/buttonStyles";
import { fetchCubeList } from "../../services/cubeCobra";

export const DEFAULT_CUBE_SETTINGS: CubeDraftSettings = {
  pod_size: 8,
  pack_count: 3,
  cards_per_pack: 15,
  min_deck_size: 40,
  addable_cards: {
    policy: "StandardBasics",
    custom: [],
  },
};

export interface CubeSetupPanelProps {
  /**
   * Called when the host clicks "Start". Async return blocks the panel's
   * internal loading state until the callback resolves.
   */
  onStart: (params: {
    cubeName: string;
    cubeListText: string;
    settings: CubeDraftSettings;
  }) => void | Promise<void>;
  /** Custom button label; defaults to the solo cube draft localized string. */
  startLabel?: string;
  /**
   * External disabled signal (e.g. the parent is busy fetching pool data).
   * Composed with the panel's internal loading state via OR.
   */
  disabled?: boolean;
}

export function CubeSetupPanel({ onStart, startLabel, disabled }: CubeSetupPanelProps) {
  const { t } = useTranslation("draft");
  const [cubeName, setCubeName] = useState(t("cubeSetup.defaultCubeName"));
  const [cubeText, setCubeText] = useState("");
  const [cubeUrl, setCubeUrl] = useState("");
  const [settings, setSettings] = useState<CubeDraftSettings>(DEFAULT_CUBE_SETTINGS);
  const [customAddables, setCustomAddables] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const updateSetting = (key: keyof Omit<CubeDraftSettings, "addable_cards">, value: number) => {
    setSettings((prev) => ({ ...prev, [key]: value }));
  };

  const updateCustomAddables = (value: string) => {
    setCustomAddables(value);
    setSettings((prev) => ({
      ...prev,
      addable_cards: {
        ...prev.addable_cards,
        custom: value
          .split("\n")
          .map((line) => line.trim())
          .filter(Boolean),
      },
    }));
  };

  const handleFetchUrl = async () => {
    if (!cubeUrl.trim()) return;
    setLoading(true);
    setError(null);
    try {
      setCubeText(await fetchCubeList(cubeUrl));
    } catch (err) {
      setError(err instanceof Error ? err.message : t("cubeSetup.fetchError"));
    } finally {
      setLoading(false);
    }
  };

  const handleStart = async () => {
    setLoading(true);
    setError(null);
    try {
      await onStart({ cubeName, cubeListText: cubeText, settings });
    } catch (err) {
      setError(err instanceof Error ? err.message : t("cubeSetup.startError"));
    } finally {
      setLoading(false);
    }
  };

  const busy = loading || disabled === true;
  const canStart = cubeText.trim().length > 0 && !busy;

  const addablesPolicyItems = useMemo(
    () => [
      { value: "StandardBasics", label: t("cubeSetup.addablesStandardBasics") },
      { value: "StandardBasicsPlusCustom", label: t("cubeSetup.addablesBasicsPlusCustom") },
      { value: "CustomOnly", label: t("cubeSetup.addablesCustomOnly") },
    ],
    [t],
  );
  const selectedAddablesLabel =
    addablesPolicyItems.find((item) => item.value === settings.addable_cards.policy)?.label ??
    t("cubeSetup.addablesStandardBasics");

  return (
    <div className="flex min-w-0 flex-col gap-4">
      <div className="grid gap-3 md:grid-cols-[1fr_220px_220px_220px]">
        <label className="flex flex-col gap-1">
          <span className="text-xs uppercase tracking-[0.16em] text-white/35">{t("cubeSetup.cubeName")}</span>
          <input
            value={cubeName}
            onChange={(e) => setCubeName(e.target.value)}
            className="rounded-lg border border-white/10 bg-black/30 px-3 py-2 text-sm text-white outline-none focus:border-emerald-400/50"
          />
        </label>
        <NumberField label={t("cubeSetup.seats")} value={settings.pod_size} min={2} max={16} onChange={(v) => updateSetting("pod_size", v)} />
        <NumberField label={t("cubeSetup.packs")} value={settings.pack_count} min={1} max={6} onChange={(v) => updateSetting("pack_count", v)} />
        <NumberField label={t("cubeSetup.packSize")} value={settings.cards_per_pack} min={1} max={30} onChange={(v) => updateSetting("cards_per_pack", v)} />
      </div>

      <div className="grid gap-3 md:grid-cols-[220px_1fr_auto]">
        <NumberField label={t("cubeSetup.minDeck")} value={settings.min_deck_size} min={1} max={100} onChange={(v) => updateSetting("min_deck_size", v)} />
        <label className="flex flex-col gap-1">
          <span className="text-xs uppercase tracking-[0.16em] text-white/35">{t("cubeSetup.exportUrl")}</span>
          <input
            value={cubeUrl}
            onChange={(e) => setCubeUrl(e.target.value)}
            placeholder={t("cubeSetup.exportUrlPlaceholder")}
            className="rounded-lg border border-white/10 bg-black/30 px-3 py-2 text-sm text-white outline-none placeholder:text-white/25 focus:border-emerald-400/50"
          />
        </label>
        <button
          type="button"
          onClick={handleFetchUrl}
          disabled={busy || !cubeUrl.trim()}
          className={menuButtonClass({ tone: "neutral", size: "md", disabled: busy || !cubeUrl.trim(), className: "self-end" })}
        >
          {t("cubeSetup.loadUrl")}
        </button>
      </div>

      <div className="grid gap-3 md:grid-cols-[260px_1fr]">
        <div className="flex flex-col gap-1">
          <span className="text-xs uppercase tracking-[0.16em] text-white/35">{t("cubeSetup.deckAddables")}</span>
          <MenuSelect
            ariaLabel={t("cubeSetup.deckAddables")}
            label={selectedAddablesLabel}
            selectedValue={settings.addable_cards.policy}
            items={addablesPolicyItems}
            onSelect={(value) =>
              setSettings((prev) => ({
                ...prev,
                addable_cards: {
                  ...prev.addable_cards,
                  policy: value as CubeDraftSettings["addable_cards"]["policy"],
                },
              }))
            }
            menuLayout="dropdown"
            fitContainer
            wrapperClassName="w-full min-w-0"
            className="min-h-[44px] rounded-lg border border-white/10 bg-black/30 px-3 py-2 text-sm text-white outline-none focus:border-emerald-400/50 sm:min-h-0"
          />
        </div>
        <label className="flex flex-col gap-1">
          <span className="text-xs uppercase tracking-[0.16em] text-white/35">{t("cubeSetup.customAddableCards")}</span>
          <textarea
            value={customAddables}
            onChange={(e) => updateCustomAddables(e.target.value)}
            placeholder={t("cubeSetup.customAddablePlaceholder")}
            className="min-h-10 resize-y rounded-lg border border-white/10 bg-black/30 px-3 py-2 text-sm text-white outline-none placeholder:text-white/25 focus:border-emerald-400/50"
          />
        </label>
      </div>

      <textarea
        value={cubeText}
        onChange={(e) => setCubeText(e.target.value)}
        spellCheck={false}
        placeholder="1 Lightning Bolt&#10;1 Black Lotus&#10;1 Tropical Island"
        className="min-h-[280px] resize-y rounded-lg border border-white/10 bg-black/35 p-3 font-mono text-sm leading-6 text-white outline-none placeholder:text-white/25 focus:border-emerald-400/50"
      />

      {error && <div className="rounded-lg border border-red-400/30 bg-red-500/10 px-3 py-2 text-sm text-red-200">{error}</div>}

      <div className="flex justify-end">
        <button
          type="button"
          onClick={handleStart}
          disabled={!canStart}
          className={menuButtonClass({ tone: "emerald", size: "lg", disabled: !canStart })}
        >
          {startLabel ?? t("cubeSetup.startCubeDraft")}
        </button>
      </div>
    </div>
  );
}

function NumberField({
  label,
  value,
  min,
  max,
  onChange,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  onChange: (value: number) => void;
}) {
  return (
    <label className="flex flex-col gap-1">
      <span className="text-xs uppercase tracking-[0.16em] text-white/35">{label}</span>
      <input
        type="number"
        min={min}
        max={max}
        value={value}
        onChange={(e) => onChange(Math.min(max, Math.max(min, Number(e.target.value))))}
        className="rounded-lg border border-white/10 bg-black/30 px-3 py-2 text-sm text-white outline-none focus:border-emerald-400/50"
      />
    </label>
  );
}
