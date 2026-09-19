import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { useLlmStore } from "../../stores/llmStore";
import { loadProviderCatalog } from "../../services/llm/catalog";
import {
  useLlmConnectionTest,
  type LlmTestState,
} from "../../hooks/useLlmConnectionTest";
import type {
  LlmProfile,
  LlmProviderCatalogEntry,
  LlmProviderId,
} from "../../services/llm/types";
import { MenuSelect } from "../ui/MenuSelect";

/**
 * Configure LLM opponents.
 *
 * LLM opponents are off unless a player builds a profile here and enables it —
 * and then binds it to a seat (in the AI opponent setup) or to draft bots (the
 * toggle below). With no profile, no LLM code path runs anywhere in the app.
 *
 * The provider list, default endpoints, and suggested models all come from the
 * engine (`phase_llm::catalog`), so this component renders a catalog rather
 * than carrying one. The free-text model field is the forward-compatibility
 * path: a model released after this build ships works by typing its id.
 */

// 44px is the minimum comfortable touch target; every interactive control in
// this panel meets it, since Settings is reachable on phones.
const TOUCH_TARGET = "min-h-[44px]";
const FIELD_CLASS =
  `${TOUCH_TARGET} w-full rounded-lg border border-white/10 bg-black/30 px-3 py-2 text-sm text-slate-100 placeholder:text-slate-500 focus:border-sky-400/60 focus:outline-none`;
const MENU_CLASS = `${TOUCH_TARGET} rounded-lg border border-white/10 bg-black/30 px-3 py-2 text-sm`;

/** Sentinel for "type a model id I don't have in the list". */
const CUSTOM_MODEL = "__custom__";


export function LlmOpponentsSection() {
  const { t } = useTranslation("settings");
  const profiles = useLlmStore((s) => s.profiles);
  const addProfile = useLlmStore((s) => s.addProfile);
  const removeProfile = useLlmStore((s) => s.removeProfile);
  const updateProfile = useLlmStore((s) => s.updateProfile);
  const draftEnabled = useLlmStore((s) => s.draftEnabled);
  const setDraftEnabled = useLlmStore((s) => s.setDraftEnabled);
  const draftProfileId = useLlmStore((s) => s.draftProfileId);
  const setDraftProfileId = useLlmStore((s) => s.setDraftProfileId);
  const catalog = useProviderCatalog();

  const usableProfiles = profiles.filter((profile) => profile.enabled && profile.model.trim());

  return (
    <div className="flex flex-col gap-4">
      <p className="rounded-lg border border-sky-400/20 bg-sky-500/5 px-3 py-2.5 text-xs leading-relaxed text-slate-300">
        {t("llm.intro")}
      </p>

      {profiles.length === 0 && (
        <p className="text-xs text-slate-500">{t("llm.empty")}</p>
      )}

      <div className="flex flex-col gap-3">
        {profiles.map((profile) => (
          <ProfileCard
            key={profile.id}
            profile={profile}
            catalog={catalog}
            onChange={(patch) => updateProfile(profile.id, patch)}
            onRemove={() => removeProfile(profile.id)}
          />
        ))}
      </div>

      <button
        type="button"
        onClick={() => addProfile()}
        className={`${TOUCH_TARGET} self-start rounded-lg border border-sky-400/40 bg-sky-500/10 px-3 py-2 text-sm font-medium text-sky-100 transition-colors hover:bg-sky-500/20`}
      >
        {t("llm.addProvider")}
      </button>

      <div className="flex flex-col gap-2 rounded-lg border border-white/8 bg-black/20 px-3 py-2.5">
        <div className="flex items-center justify-between gap-3">
          <div className="flex min-w-0 flex-col">
            <span className="text-xs font-semibold text-slate-200">{t("llm.draftToggle.label")}</span>
            <span className="text-[10px] leading-relaxed text-slate-400">
              {t("llm.draftToggle.hint")}
            </span>
          </div>
          <button
            type="button"
            role="switch"
            aria-checked={draftEnabled}
            aria-label={t("llm.draftToggle.label")}
            disabled={usableProfiles.length === 0}
            onClick={() => setDraftEnabled(!draftEnabled)}
            className={`relative inline-flex h-6 w-11 shrink-0 items-center rounded-full transition-colors disabled:opacity-40 ${
              draftEnabled ? "bg-sky-500" : "bg-white/15"
            }`}
          >
            <span
              className={`inline-block h-5 w-5 transform rounded-full bg-white transition-transform ${
                draftEnabled ? "translate-x-5" : "translate-x-0.5"
              }`}
            />
          </button>
        </div>
        {draftEnabled && usableProfiles.length > 0 && (
          <MenuSelect
            ariaLabel={t("llm.draftToggle.profile")}
            label={
              usableProfiles.find((profile) => profile.id === draftProfileId)?.name
              || profileLabel(usableProfiles[0], t("llm.unnamed"))
            }
            selectedValue={draftProfileId ?? usableProfiles[0].id}
            items={usableProfiles.map((profile) => ({
              value: profile.id,
              label: profileLabel(profile, t("llm.unnamed")),
            }))}
            onSelect={setDraftProfileId}
            menuLayout="dropdown"
            fitContainer
            className={MENU_CLASS}
          />
        )}
      </div>
    </div>
  );
}

function profileLabel(profile: LlmProfile, fallback: string): string {
  return profile.name.trim() || profile.model.trim() || fallback;
}

function ProfileCard({
  profile,
  catalog,
  onChange,
  onRemove,
}: {
  profile: LlmProfile;
  catalog: LlmProviderCatalogEntry[];
  onChange: (patch: Partial<LlmProfile>) => void;
  onRemove: () => void;
}) {
  const { t } = useTranslation("settings");
  const [test, setTest] = useState<LlmTestState>({ status: "idle" });
  // Local draft so the endpoint commits once, on blur, rather than per
  // keystroke. Re-synced whenever the stored value changes underneath (a
  // provider switch resets it to the new default).
  const [endpointDraft, setEndpointDraft] = useState(profile.baseUrl ?? "");
  useEffect(() => {
    setEndpointDraft(profile.baseUrl ?? "");
  }, [profile.baseUrl]);
  const commitEndpoint = useCallback(() => {
    const next = endpointDraft.trim() || null;
    if ((next ?? "") === (profile.baseUrl ?? "")) return;
    onChange({ baseUrl: next });
  }, [endpointDraft, profile.baseUrl, onChange]);
  const entry = catalog.find((row) => row.provider === profile.provider);
  const suggestedModels = entry?.models ?? [];
  // A model the player typed is "custom" precisely when the catalog does not
  // list it — including every model for a provider with no suggestions.
  const isCustomModel =
    !profile.model || !suggestedModels.some((model) => model.id === profile.model);
  const runTest = useLlmConnectionTest(profile, setTest);

  const onProviderChange = (value: string) => {
    const next = value as LlmProviderId;
    const nextEntry = catalog.find((row) => row.provider === next);
    // Switching vendor invalidates the model, the endpoint AND the credential
    // together. A model id is vendor-specific, and a key is issued by one
    // vendor -- carrying it over would send an OpenAI key to Anthropic at the
    // first decision. The store enforces this too; clearing it here keeps the
    // field visibly empty rather than showing a stale masked value.
    onChange({
      provider: next,
      model: nextEntry?.defaultModel ?? "",
      baseUrl: null,
      apiKey: "",
      enabled: false,
    });
  };

  return (
    <div className="flex flex-col gap-3 rounded-[14px] border border-white/10 bg-black/25 p-3">
      <div className="flex items-center gap-2">
        <input
          type="text"
          value={profile.name}
          placeholder={t("llm.namePlaceholder")}
          aria-label={t("llm.name")}
          onChange={(e) => onChange({ name: e.target.value })}
          className={FIELD_CLASS}
        />
        <button
          type="button"
          role="switch"
          aria-checked={profile.enabled}
          aria-label={t("llm.enable")}
          onClick={() => onChange({ enabled: !profile.enabled })}
          className={`relative inline-flex h-6 w-11 shrink-0 items-center rounded-full transition-colors ${
            profile.enabled ? "bg-emerald-500" : "bg-white/15"
          }`}
        >
          <span
            className={`inline-block h-5 w-5 transform rounded-full bg-white transition-transform ${
              profile.enabled ? "translate-x-5" : "translate-x-0.5"
            }`}
          />
        </button>
        <button
          type="button"
          onClick={onRemove}
          aria-label={t("llm.remove")}
          className={`${TOUCH_TARGET} shrink-0 rounded-lg border border-rose-400/30 px-2.5 text-sm text-rose-200 transition-colors hover:bg-rose-500/15`}
        >
          ✕
        </button>
      </div>

      <Field label={t("llm.provider")}>
        <MenuSelect
          ariaLabel={t("llm.provider")}
          label={entry?.displayName ?? profile.provider}
          selectedValue={profile.provider}
          items={catalog.map((row) => ({ value: row.value, label: row.displayName }))}
          onSelect={onProviderChange}
          menuLayout="dropdown"
          fitContainer
          className={MENU_CLASS}
        />
      </Field>

      <Field label={t("llm.model")}>
        {suggestedModels.length > 0 && (
          <MenuSelect
            ariaLabel={t("llm.model")}
            label={
              isCustomModel
                ? t("llm.customModel")
                : (suggestedModels.find((model) => model.id === profile.model)?.label
                  ?? profile.model)
            }
            selectedValue={isCustomModel ? CUSTOM_MODEL : profile.model}
            items={[
              ...suggestedModels.map((model) => ({ value: model.id, label: model.label })),
              { value: CUSTOM_MODEL, label: t("llm.customModel") },
            ]}
            onSelect={(value) => onChange({ model: value === CUSTOM_MODEL ? "" : value })}
            menuLayout="dropdown"
            fitContainer
            className={MENU_CLASS}
          />
        )}
        {(isCustomModel || suggestedModels.length === 0) && (
          <input
            type="text"
            value={profile.model}
            placeholder={t("llm.modelPlaceholder")}
            aria-label={t("llm.modelId")}
            onChange={(e) => onChange({ model: e.target.value })}
            className={`${FIELD_CLASS} mt-2`}
          />
        )}
      </Field>

      <Field label={t("llm.endpoint")}>
        <input
          type="url"
          inputMode="url"
          value={endpointDraft}
          placeholder={entry?.defaultBaseUrl ?? t("llm.endpointPlaceholder")}
          aria-label={t("llm.endpoint")}
          // Committed on blur (or Enter), never per keystroke. Changing the
          // endpoint clears the credential — it is scoped to the server it was
          // issued for — and committing on every character would wipe the key on
          // the first one typed.
          onChange={(e) => setEndpointDraft(e.target.value)}
          onBlur={commitEndpoint}
          onKeyDown={(e) => {
            if (e.key === "Enter") commitEndpoint();
          }}
          className={FIELD_CLASS}
        />
      </Field>

      <Field label={t("llm.apiKey")}>
        <input
          type="password"
          autoComplete="off"
          spellCheck={false}
          value={profile.apiKey}
          placeholder={entry?.requiresApiKey ? t("llm.apiKeyPlaceholder") : t("llm.apiKeyOptional")}
          aria-label={t("llm.apiKey")}
          onChange={(e) => onChange({ apiKey: e.target.value })}
          className={FIELD_CLASS}
        />
        <p className="mt-1 text-[10px] leading-relaxed text-slate-500">
          {t("llm.apiKeyStorage")}
          {entry?.apiKeyUrl ? (
            <>
              {" "}
              <a
                href={entry.apiKeyUrl}
                target="_blank"
                rel="noreferrer noopener"
                className="text-sky-300 underline"
              >
                {t("llm.apiKeyGet")}
              </a>
            </>
          ) : null}
        </p>
      </Field>

      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={runTest}
          disabled={test.status === "running" || !profile.model.trim()}
          className={`${TOUCH_TARGET} rounded-lg border border-white/12 bg-white/5 px-3 text-xs font-medium text-slate-200 transition-colors hover:bg-white/10 disabled:opacity-40`}
        >
          {test.status === "running" ? t("llm.testing") : t("llm.test")}
        </button>
        {test.status === "ok" && (
          <span className="text-xs text-emerald-300">{t("llm.testOk")}</span>
        )}
        {test.status === "failed" && (
          <span className="min-w-0 break-words text-xs text-amber-300">
            {/* The reason is translated; the provider's own diagnostic is
                appended verbatim, because it is data and usually the useful
                half ("Incorrect API key provided"). */}
            {t(`llm.errors.${test.code}`)}
            {test.detail ? ` ${test.detail}` : ""}
          </span>
        )}
        {test.status === "idle" && (
          <span className="min-w-0 text-[10px] text-slate-500">{t("llm.testHint")}</span>
        )}
      </div>
    </div>
  );
}

/** The engine-owned provider catalog. */
function useProviderCatalog(): LlmProviderCatalogEntry[] {
  const [catalog, setCatalog] = useState<LlmProviderCatalogEntry[]>([]);

  useEffect(() => {
    let cancelled = false;
    void loadProviderCatalog().then((rows) => {
      if (!cancelled) setCatalog(rows);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  return useMemo(() => catalog, [catalog]);
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <span className="mb-1.5 block text-[0.62rem] font-semibold uppercase tracking-[0.16em] text-slate-500">
        {label}
      </span>
      {children}
    </div>
  );
}
