import { useEffect, useId, useMemo, useRef, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";

import type { GameAction, GameState, WaitingFor } from "../../adapter/types.ts";
import {
  copyGameStateDebugSnapshot,
  exportGameStateDebugZip,
} from "../../services/gameStateExport.ts";
import { useCanActForWaitingState, usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";

interface HelpEntry {
  title: string;
  body: string;
  section: "Flow" | "Shortcuts" | "Recovery";
  shortcut?: string;
}

const HELP_ENTRIES: HelpEntry[] = [
  {
    section: "Flow",
    title: "Automatic phase skips",
    body: "Phase skips are automatic. Use stops or Full Control when you want paper-style priority windows.",
  },
  {
    section: "Flow",
    title: "Phase stops",
    body: "A stop pauses before that step, similar to saying you may act before combat, before draw, or before the end step in paper.",
  },
  {
    section: "Flow",
    title: "Full Control",
    body: "Full Control keeps priority windows from being skipped, including spots where the digital client would normally keep the game moving.",
    shortcut: "F",
  },
  {
    section: "Flow",
    title: "Resolve",
    body: "Resolve means you pass priority so the top spell or ability on the stack can resolve if everyone else also passes.",
    shortcut: "Space",
  },
  {
    section: "Flow",
    title: "Pass to End",
    body: "Pass to End keeps passing priority for the turn unless a choice, stop, or Full Control interrupts.",
    shortcut: "Enter",
  },
  {
    section: "Flow",
    title: "Mana payment",
    body: "During mana payment, you can tap lands manually or press T to tap available lands.",
    shortcut: "T",
  },
  {
    section: "Flow",
    title: "Combat declarations",
    body: "The game asks for attackers and blockers only during the declaration steps. Choose creatures, then confirm the declaration.",
  },
  {
    section: "Shortcuts",
    title: "Open Help",
    body: "Open this help sheet.",
    shortcut: "?",
  },
  {
    section: "Shortcuts",
    title: "Pass priority",
    body: "Pass priority or advance through the current priority prompt.",
    shortcut: "Space",
  },
  {
    section: "Shortcuts",
    title: "Undo",
    body: "Undo the last local action that did not reveal hidden information.",
    shortcut: "Z",
  },
  {
    section: "Shortcuts",
    title: "Cancel",
    body: "Cancel the current selection, mana payment, target selection, or auto-pass when available.",
    shortcut: "Esc",
  },
  {
    section: "Shortcuts",
    title: "Advanced debug panel",
    body: "Open the advanced debug panel. Most players should start with Recovery Tools first.",
    shortcut: "`",
  },
  {
    section: "Recovery",
    title: "Report or export state",
    body: "If a card misbehaves, export the current game state so the exact board position can be reproduced.",
  },
  {
    section: "Recovery",
    title: "Board right-click menu",
    body: "On desktop, right-click empty board space for the game log, recovery/debug tools, and background settings.",
  },
];

const SECTION_ORDER: HelpEntry["section"][] = ["Flow", "Shortcuts", "Recovery"];

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
}: {
  waitingFor: WaitingFor | null;
  gameState: GameState | null;
  playerId: number;
  canActForWaitingState: boolean;
  legalActions: GameAction[];
  legalActionsByObject: Record<string, GameAction[]>;
  autoPassRecommended: boolean;
}): string {
  if (!waitingFor || !gameState) return "The game is starting or restoring state.";
  if (waitingFor.type === "GameOver") return "The game is over.";

  if (waitingFor.type === "MulliganDecision") {
    return waitingFor.data.pending.some((entry) => entry.player === playerId)
      ? "Choose whether to keep this opening hand or take a mulligan."
      : "Waiting for another player to decide their opening hand.";
  }

  if (waitingFor.type === "MulliganBottomCards") {
    return waitingFor.data.pending.some((entry) => entry.player === playerId)
      ? "Choose cards to put on the bottom after keeping a mulligan hand."
      : "Waiting for another player to finish their mulligan.";
  }

  if (waitingFor.type === "OpeningHandBottomCards") {
    return waitingFor.data.pending.some((entry) => entry.player === playerId)
      ? "Choose a card to put on the bottom before normal mulligans begin."
      : "Waiting for another player to resolve their opening hand.";
  }

  if (!canActForWaitingState) {
    return "Waiting for another player to act.";
  }

  switch (waitingFor.type) {
    case "Priority": {
      const castCount = actionCount(legalActions, "CastSpell");
      const abilityCount = actionCount(legalActions, "ActivateAbility");
      const objectCount = Object.keys(legalActionsByObject).length;
      if (gameState.stack.length > 0) {
        return "You have priority with something on the stack. Resolve passes priority so the top item can resolve.";
      }
      if (autoPassRecommended) {
        return "You have priority. The client may auto-pass quiet windows unless a stop or Full Control is on.";
      }
      if (castCount > 0 || abilityCount > 0 || objectCount > 0) {
        return "You have priority. You can use available cards or pass to keep the turn moving.";
      }
      return "You have priority. Passing continues to the next step or player.";
    }
    case "ManaPayment":
      return "Pay mana for the pending spell or ability. Press T to tap available lands.";
    case "TargetSelection":
    case "TriggerTargetSelection":
    case "CopyTargetChoice":
    case "CopyRetarget":
      return "Choose the highlighted legal target or cancel if the prompt allows it.";
    case "DeclareAttackers":
      return "Choose attackers, then confirm attackers. You can also attack with none.";
    case "DeclareBlockers":
      return "Choose blockers and assign them to attackers, then confirm blockers.";
    case "ChooseXValue":
      return "Choose a value for X before continuing with the spell or ability.";
    case "PayAmountChoice":
      return "Choose how much of the requested resource to pay.";
    default:
      return "The game is waiting for your choice. Follow the active prompt to continue.";
  }
}

export function HelpSheet() {
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
  });

  const filteredEntries = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return HELP_ENTRIES;
    return HELP_ENTRIES.filter((entry) =>
      [entry.section, entry.title, entry.body, entry.shortcut ?? ""]
        .join(" ")
        .toLowerCase()
        .includes(needle),
    );
  }, [query]);

  const entriesBySection = SECTION_ORDER.map((section) => ({
    section,
    entries: filteredEntries.filter((entry) => entry.section === section),
  })).filter((group) => group.entries.length > 0);

  const handleCopyState = () => {
    if (!gameState) return;
    copyGameStateDebugSnapshot(gameState)
      .then(() => setStatus("Copied game state to clipboard."))
      .catch(() => setStatus("Could not copy game state."));
  };

  const handleExportState = () => {
    if (!gameState) return;
    exportGameStateDebugZip(gameState)
      .then((filename) => setStatus(`Exported ${filename}.`))
      .catch((err: unknown) => {
        if (err instanceof DOMException && err.name === "AbortError") return;
        setStatus("Could not export game state.");
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
                    Help
                  </div>
                  <h2 id={titleId} className="mt-1 text-xl font-semibold text-white">
                    Help & Shortcuts
                  </h2>
                  <p className="mt-1 text-sm text-slate-400">
                    Digital Magic flow for paper players: stops, priority, passing, and recovery tools.
                  </p>
                </div>
                <button
                  type="button"
                  onClick={() => setOpen(false)}
                  className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full border border-white/10 bg-white/5 text-xl leading-none text-slate-300 transition hover:bg-white/10 hover:text-white"
                  aria-label="Close help"
                >
                  &times;
                </button>
              </div>
              <input
                ref={searchRef}
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder="Search help or shortcuts"
                className="mt-4 h-11 w-full rounded-xl border border-white/10 bg-black/24 px-3 text-sm text-white outline-none transition placeholder:text-slate-500 focus:border-cyan-400/50 focus:ring-2 focus:ring-cyan-400/20"
              />
            </header>

            <div className="min-h-0 flex-1 overflow-y-auto px-4 py-4 lg:px-5">
              <section className="mb-4 rounded-xl border border-cyan-300/20 bg-cyan-400/10 p-4">
                <div className="text-[0.68rem] font-semibold uppercase tracking-[0.2em] text-cyan-200/80">
                  What can I do now?
                </div>
                <p className="mt-2 text-sm leading-6 text-slate-100">{summary}</p>
              </section>

              <div className="space-y-5">
                {entriesBySection.map((group) => (
                  <section key={group.section}>
                    <h3 className="mb-2 text-[0.68rem] font-semibold uppercase tracking-[0.22em] text-slate-500">
                      {group.section}
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
                  Recovery Tools
                </h3>
                <p className="mt-2 text-sm leading-6 text-slate-200">
                  If a card misbehaves, export state first. The advanced debug panel is available when you need to inspect or adjust the game.
                </p>
                <div className="mt-3 flex flex-wrap gap-2">
                  <button
                    type="button"
                    disabled={!gameState}
                    onClick={handleCopyState}
                    className="rounded-lg border border-white/10 bg-white/8 px-3 py-2 text-sm font-semibold text-slate-100 transition hover:bg-white/12 disabled:cursor-not-allowed disabled:opacity-40"
                  >
                    Copy State
                  </button>
                  <button
                    type="button"
                    disabled={!gameState}
                    onClick={handleExportState}
                    className="rounded-lg border border-white/10 bg-white/8 px-3 py-2 text-sm font-semibold text-slate-100 transition hover:bg-white/12 disabled:cursor-not-allowed disabled:opacity-40"
                  >
                    Export State
                  </button>
                  <button
                    type="button"
                    onClick={() => {
                      setOpen(false);
                      toggleDebugPanel();
                    }}
                    className="rounded-lg border border-white/10 bg-white/8 px-3 py-2 text-sm font-semibold text-slate-100 transition hover:bg-white/12"
                  >
                    Open Advanced Debug
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
