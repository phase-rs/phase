import { useEffect, useId, useMemo, useRef, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";

import type { GameAction, GameState, WaitingFor } from "../../adapter/types.ts";
import {
  copyGameStateDebugSnapshot,
  exportGameStateDebugZip,
} from "../../services/gameStateExport.ts";
import { useCanActForWaitingState, usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";

type HelpSection = "Flow" | "Shortcuts" | "Recovery";

interface HelpEntry {
  id: string;
  section: HelpSection;
  shortcut?: string;
}

interface ResolvedHelpEntry extends HelpEntry {
  title: string;
  body: string;
}

const HELP_ENTRIES: HelpEntry[] = [
  { section: "Flow", id: "automaticPhaseSkips" },
  { section: "Flow", id: "phaseStops" },
  { section: "Flow", id: "fullControl", shortcut: "F" },
  { section: "Flow", id: "resolve", shortcut: "Space" },
  { section: "Flow", id: "passToEnd", shortcut: "Enter" },
  { section: "Flow", id: "manaPayment", shortcut: "T" },
  { section: "Flow", id: "combatDeclarations" },
  { section: "Shortcuts", id: "openHelp", shortcut: "?" },
  { section: "Shortcuts", id: "passPriority", shortcut: "Space" },
  { section: "Shortcuts", id: "undo", shortcut: "Z" },
  { section: "Shortcuts", id: "cancel", shortcut: "Esc" },
  { section: "Shortcuts", id: "advancedDebugPanel", shortcut: "`" },
  { section: "Recovery", id: "reportOrExportState" },
  { section: "Recovery", id: "boardRightClickMenu" },
];

const SECTION_ORDER: HelpSection[] = ["Flow", "Shortcuts", "Recovery"];

function actionCount(actions: GameAction[], type: GameAction["type"]): number {
  return actions.filter((action) => action.type === type).length;
}

function currentPromptSummary({
  waitingFor,
  gameState,
  playerId,
  canActForWaitingState,
  legalActions,
  legalActionsByObject,
  autoPassRecommended,
  t,
}: {
  waitingFor: WaitingFor | null;
  gameState: GameState | null;
  playerId: number;
  canActForWaitingState: boolean;
  legalActions: GameAction[];
  legalActionsByObject: Record<string, GameAction[]>;
  autoPassRecommended: boolean;
  t: TFunction;
}): string {
  if (!waitingFor || !gameState) return t("help.prompt.starting");
  if (waitingFor.type === "GameOver") return t("help.prompt.gameOver");

  if (waitingFor.type === "MulliganDecision") {
    const entry = waitingFor.data.pending.find((e) => e.player === playerId);
    if (entry) {
      return entry.phase.type === "BottomCards"
        ? t("help.prompt.mulliganBottom")
        : t("help.prompt.mulliganDecide");
    }
    const someoneBottoming = waitingFor.data.pending.some(
      (e) => e.phase.type === "BottomCards",
    );
    return someoneBottoming
      ? t("help.prompt.mulliganWaitBottom")
      : t("help.prompt.mulliganWaitDecide");
  }

  if (waitingFor.type === "OpeningHandBottomCards") {
    return waitingFor.data.pending.some((entry) => entry.player === playerId)
      ? t("help.prompt.openingHandBottom")
      : t("help.prompt.openingHandWait");
  }

  if (!canActForWaitingState) {
    return t("help.prompt.waitOther");
  }

  switch (waitingFor.type) {
    case "Priority": {
      const castCount = actionCount(legalActions, "CastSpell");
      const abilityCount = actionCount(legalActions, "ActivateAbility");
      const objectCount = Object.keys(legalActionsByObject).length;
      if (gameState.stack.length > 0) {
        return t("help.prompt.priorityStack");
      }
      if (autoPassRecommended) {
        return t("help.prompt.priorityAutoPass");
      }
      if (castCount > 0 || abilityCount > 0 || objectCount > 0) {
        return t("help.prompt.priorityActions");
      }
      return t("help.prompt.priorityPass");
    }
    case "ManaPayment":
      return t("help.prompt.manaPayment");
    case "TargetSelection":
    case "TriggerTargetSelection":
    case "CopyTargetChoice":
    case "CopyRetarget":
    case "ReturnAsAuraTarget":
    case "ExploreChoice":
    case "PopulateChoice":
      return t("help.prompt.targetSelection");
    case "DeclareAttackers":
      return t("help.prompt.declareAttackers");
    case "DeclareBlockers":
      return t("help.prompt.declareBlockers");
    case "ChooseXValue":
      return t("help.prompt.chooseXValue");
    case "PayAmountChoice":
      return t("help.prompt.payAmountChoice");
    default:
      return t("help.prompt.default");
  }
}

export function HelpSheet() {
  const { t } = useTranslation();
  const open = useUiStore((s) => s.helpSheetOpen);
  const setOpen = useUiStore((s) => s.setHelpSheetOpen);
  const toggleDebugPanel = useUiStore((s) => s.toggleDebugPanel);
  const gameState = useGameStore((s) => s.gameState);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const legalActions = useGameStore((s) => s.legalActions);
  const legalActionsByObject = useGameStore((s) => s.legalActionsByObject);
  const autoPassRecommended = useGameStore((s) => s.autoPassRecommended);
  const playerId = usePlayerId();
  const canActForWaitingState = useCanActForWaitingState();
  const [query, setQuery] = useState("");
  const [status, setStatus] = useState<string | null>(null);
  const panelRef = useRef<HTMLDivElement | null>(null);
  const searchRef = useRef<HTMLInputElement | null>(null);
  const restoreFocusRef = useRef<HTMLElement | null>(null);
  const titleId = useId();

  useEffect(() => {
    if (!open) return;
    restoreFocusRef.current = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
    requestAnimationFrame(() => searchRef.current?.focus());

    return () => {
      restoreFocusRef.current?.focus();
      restoreFocusRef.current = null;
    };
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        setOpen(false);
        return;
      }
      if (event.key !== "Tab") return;

      const focusable = panelRef.current?.querySelectorAll<HTMLElement>(
        "button, [href], input, select, textarea, [tabindex]:not([tabindex='-1'])",
      );
      if (!focusable || focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };

    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, setOpen]);

  const summary = currentPromptSummary({
    waitingFor,
    gameState,
    playerId,
    canActForWaitingState,
    legalActions,
    legalActionsByObject,
    autoPassRecommended,
    t,
  });

  const filteredEntries = useMemo<ResolvedHelpEntry[]>(() => {
    const resolved = HELP_ENTRIES.map((entry) => ({
      ...entry,
      title: t(`help.entries.${entry.id}.title`),
      body: t(`help.entries.${entry.id}.body`),
    }));
    const needle = query.trim().toLowerCase();
    if (!needle) return resolved;
    return resolved.filter((entry) =>
      [t(`help.sections.${entry.section}`), entry.title, entry.body, entry.shortcut ?? ""]
        .join(" ")
        .toLowerCase()
        .includes(needle),
    );
  }, [query, t]);

  const entriesBySection = SECTION_ORDER.map((section) => ({
    section,
    entries: filteredEntries.filter((entry) => entry.section === section),
  })).filter((group) => group.entries.length > 0);

  const handleCopyState = () => {
    if (!gameState) return;
    copyGameStateDebugSnapshot(gameState)
      .then(() => setStatus(t("help.status.copied")))
      .catch(() => setStatus(t("help.status.copyFailed")));
  };

  const handleExportState = () => {
    if (!gameState) return;
    exportGameStateDebugZip(gameState)
      .then((filename) => setStatus(t("help.status.exported", { filename })))
      .catch((err: unknown) => {
        if (err instanceof DOMException && err.name === "AbortError") return;
        setStatus(t("help.status.exportFailed"));
      });
  };

  return (
    <AnimatePresence>
      {open && (
        <motion.div
          className="fixed inset-0 z-[120] flex items-end justify-center bg-black/40 px-0 pt-[env(safe-area-inset-top)] backdrop-blur-sm lg:items-center lg:px-4 lg:py-6"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.18 }}
        >
          <div
            className="absolute inset-0 cursor-default"
            onClick={() => setOpen(false)}
            aria-hidden="true"
          />
          <motion.div
            ref={panelRef}
            role="dialog"
            aria-modal="true"
            aria-labelledby={titleId}
            className="relative flex max-h-[88vh] w-full max-w-3xl flex-col overflow-hidden rounded-t-[18px] border border-white/10 bg-[#0b1020]/96 text-slate-100 shadow-[0_32px_90px_rgba(0,0,0,0.55)] backdrop-blur-xl lg:rounded-[18px]"
            initial={{ y: 24, opacity: 0, scale: 0.98 }}
            animate={{ y: 0, opacity: 1, scale: 1 }}
            exit={{ y: 24, opacity: 0, scale: 0.98 }}
            transition={{ duration: 0.2, ease: "easeOut" }}
          >
            <header className="border-b border-white/10 px-4 py-4 lg:px-5">
              <div className="flex items-start justify-between gap-4">
                <div>
                  <div className="text-[0.68rem] font-semibold uppercase tracking-[0.22em] text-cyan-300/80">
                    {t("help.eyebrow")}
                  </div>
                  <h2 id={titleId} className="mt-1 text-xl font-semibold text-white">
                    {t("help.title")}
                  </h2>
                  <p className="mt-1 text-sm text-slate-400">
                    {t("help.subtitle")}
                  </p>
                </div>
                <button
                  type="button"
                  onClick={() => setOpen(false)}
                  className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full border border-white/10 bg-white/5 text-xl leading-none text-slate-300 transition hover:bg-white/10 hover:text-white"
                  aria-label={t("help.closeHelp")}
                >
                  &times;
                </button>
              </div>
              <input
                ref={searchRef}
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder={t("help.searchPlaceholder")}
                className="mt-4 h-11 w-full rounded-xl border border-white/10 bg-black/24 px-3 text-sm text-white outline-none transition placeholder:text-slate-500 focus:border-cyan-400/50 focus:ring-2 focus:ring-cyan-400/20"
              />
            </header>

            <div className="min-h-0 flex-1 overflow-y-auto px-4 py-4 lg:px-5">
              <section className="mb-4 rounded-xl border border-cyan-300/20 bg-cyan-400/10 p-4">
                <div className="text-[0.68rem] font-semibold uppercase tracking-[0.2em] text-cyan-200/80">
                  {t("help.whatCanIDo")}
                </div>
                <p className="mt-2 text-sm leading-6 text-slate-100">{summary}</p>
              </section>

              <div className="space-y-5">
                {entriesBySection.map((group) => (
                  <section key={group.section}>
                    <h3 className="mb-2 text-[0.68rem] font-semibold uppercase tracking-[0.22em] text-slate-500">
                      {t(`help.sections.${group.section}`)}
                    </h3>
                    <div className="overflow-hidden rounded-xl border border-white/10 bg-black/18">
                      {group.entries.map((entry, index) => (
                        <article
                          key={`${entry.section}-${entry.title}`}
                          className={`flex min-h-16 items-start justify-between gap-4 px-4 py-3 ${
                            index > 0 ? "border-t border-white/8" : ""
                          }`}
                        >
                          <div>
                            <h4 className="text-sm font-semibold text-slate-100">{entry.title}</h4>
                            <p className="mt-1 text-sm leading-5 text-slate-400">{entry.body}</p>
                          </div>
                          {entry.shortcut && <ShortcutKey>{entry.shortcut}</ShortcutKey>}
                        </article>
                      ))}
                    </div>
                  </section>
                ))}
              </div>

              <section className="mt-5 rounded-xl border border-amber-300/20 bg-amber-400/10 p-4">
                <h3 className="text-[0.68rem] font-semibold uppercase tracking-[0.22em] text-amber-200/80">
                  {t("help.recoveryTitle")}
                </h3>
                <p className="mt-2 text-sm leading-6 text-slate-200">
                  {t("help.recoveryDescription")}
                </p>
                <div className="mt-3 flex flex-wrap gap-2">
                  <button
                    type="button"
                    disabled={!gameState}
                    onClick={handleCopyState}
                    className="rounded-lg border border-white/10 bg-white/8 px-3 py-2 text-sm font-semibold text-slate-100 transition hover:bg-white/12 disabled:cursor-not-allowed disabled:opacity-40"
                  >
                    {t("help.copyState")}
                  </button>
                  <button
                    type="button"
                    disabled={!gameState}
                    onClick={handleExportState}
                    className="rounded-lg border border-white/10 bg-white/8 px-3 py-2 text-sm font-semibold text-slate-100 transition hover:bg-white/12 disabled:cursor-not-allowed disabled:opacity-40"
                  >
                    {t("help.exportState")}
                  </button>
                  <button
                    type="button"
                    onClick={() => {
                      setOpen(false);
                      toggleDebugPanel();
                    }}
                    className="rounded-lg border border-white/10 bg-white/8 px-3 py-2 text-sm font-semibold text-slate-100 transition hover:bg-white/12"
                  >
                    {t("help.openAdvancedDebug")}
                  </button>
                </div>
                {status && <p className="mt-2 text-xs text-emerald-300">{status}</p>}
              </section>
            </div>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>
  );
}

function ShortcutKey({ children }: { children: string }) {
  return (
    <kbd className="mt-0.5 shrink-0 rounded-md border border-white/10 bg-slate-950/70 px-2 py-1 font-mono text-xs text-slate-300">
      {children}
    </kbd>
  );
}
