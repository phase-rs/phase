import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";

import type { EngineAdapter, GameEvent, GameState, PlayerId } from "../../adapter/types.ts";
import { getPlayerId, usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../stores/multiplayerStore.ts";
import { LifeTotal } from "../controls/LifeTotal.tsx";

interface GameSessionBinding {
  gameId: string | null;
  adapter: EngineAdapter;
  gameSessionGeneration: number;
}

interface LifeCorrectionDraft extends GameSessionBinding {
  targetPlayerId: PlayerId;
  value: string;
  initialLife: number;
  engineCommitEpoch: number;
  gameState: GameState;
}

interface CorrectionFeedback extends GameSessionBinding {
  status: "applied" | "notApplied" | "failed";
  targetPlayerId: PlayerId;
}

interface SubmissionToken {
  requestId: number;
  binding: GameSessionBinding;
}

interface PanelPosition {
  left: number;
  top: number;
  maxHeight: number;
}

interface TriggerPosition {
  left: number;
  top: number;
  width: number;
  height: number;
}

type StoreSnapshot = ReturnType<typeof useGameStore.getState>;

function isFullscreenBlockingOverlay(element: HTMLElement): boolean {
  if (element.closest("[data-sandbox-life-correction]")) return false;

  const style = window.getComputedStyle(element);
  if (style.position !== "fixed" || style.pointerEvents === "none") return false;
  const zIndex = Number.parseInt(style.zIndex, 10);
  if (!Number.isFinite(zIndex) || zIndex < 40) return false;

  const rect = element.getBoundingClientRect();
  return rect.left <= 0
    && rect.top <= 0
    && rect.right >= window.innerWidth
    && rect.bottom >= window.innerHeight;
}

function hasBlockingOverlay(): boolean {
  if (typeof document === "undefined" || !document.body) return false;
  if (document.querySelector(
    '[aria-modal="true"], [data-engine-lost-reason], [data-unhandled-waiting-for]',
  )) {
    return true;
  }

  return Array.from(document.querySelectorAll<HTMLElement>(".fixed.inset-0"))
    .some(isFullscreenBlockingOverlay);
}

/**
 * Keep this leaf out of modal/recovery layers that cover the game. Coachmarks
 * and other anchored hints are intentionally ignored: they do not claim the
 * whole viewport or expose modal semantics.
 */
function useBlockingOverlayActive(enabled: boolean): boolean {
  const [active, setActive] = useState(
    () => enabled && typeof document !== "undefined" && hasBlockingOverlay(),
  );

  useLayoutEffect(() => {
    if (!enabled) {
      setActive(false);
      return;
    }

    const root = document.body;
    if (!root) return;

    const update = () => {
      const next = hasBlockingOverlay();
      setActive((current) => current === next ? current : next);
    };
    update();

    const observer = new MutationObserver(update);
    observer.observe(root, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: [
        "aria-modal",
        "data-engine-lost-reason",
        "data-unhandled-waiting-for",
      ],
    });
    return () => observer.disconnect();
  }, [enabled]);

  return active;
}

function sandboxFlagEnabled(): boolean {
  return import.meta.env.DEV && Reflect.get(import.meta.env, "VITE_PHASE_SANDBOX") === "1";
}

function captureDraft(targetPlayerId: PlayerId, store: StoreSnapshot): LifeCorrectionDraft | null {
  const gameState = store.gameState;
  const adapter = store.adapter;
  const player = gameState?.players.find((entry) => entry.id === targetPlayerId);
  if (!gameState || !adapter || !player) return null;

  return {
    targetPlayerId,
    value: String(player.life),
    initialLife: player.life,
    gameId: store.gameId,
    adapter,
    gameSessionGeneration: store.gameSessionGeneration,
    engineCommitEpoch: store.engineCommitEpoch,
    gameState,
  };
}

function sameGameSession(a: GameSessionBinding, b: GameSessionBinding): boolean {
  return a.adapter === b.adapter
    && a.gameId === b.gameId
    && a.gameSessionGeneration === b.gameSessionGeneration;
}

function sessionMatchesStore(binding: GameSessionBinding, store: StoreSnapshot): boolean {
  return store.adapter === binding.adapter
    && store.gameId === binding.gameId
    && store.gameSessionGeneration === binding.gameSessionGeneration;
}

function bindingForStore(store: StoreSnapshot): GameSessionBinding | null {
  if (!store.adapter) return null;
  return {
    adapter: store.adapter,
    gameId: store.gameId,
    gameSessionGeneration: store.gameSessionGeneration,
  };
}

function draftStillMatches(draft: LifeCorrectionDraft, store: StoreSnapshot): boolean {
  return sessionMatchesStore(draft, store)
    && store.engineCommitEpoch === draft.engineCommitEpoch
    && store.gameState === draft.gameState
    && store.gameState?.players.find((player) => player.id === draft.targetPlayerId)?.life === draft.initialLife;
}

function expectedSetLifeDescription(targetPlayerId: PlayerId, expectedLife: number): string {
  // The browser WASM state keeps log_player_names runtime-only and empty, so
  // DebugAction::describe uses its exact "Player N" fallback for local/AI play.
  return `SetLife (Player ${targetPlayerId + 1} → ${expectedLife})`;
}

function confirmsCorrection(
  events: GameEvent[],
  actor: PlayerId,
  targetPlayerId: PlayerId,
  expectedLife: number,
): boolean {
  const debugActions = events.filter((event) => event.type === "DebugActionUsed");
  return debugActions.length === 1
    && debugActions[0]?.data.player_id === actor
    && debugActions[0]?.data.description === expectedSetLifeDescription(targetPlayerId, expectedLife);
}

/** Local development affordance for the sandbox walkthrough. It uses the same
 * adapter dispatch as the rest of the game and only reads committed life. */
export function SandboxLifeCorrection() {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const localPlayerId = usePlayerId();
  const gameMode = useGameStore((store) => store.gameMode);
  const gameId = useGameStore((store) => store.gameId);
  const gameSessionGeneration = useGameStore((store) => store.gameSessionGeneration);
  const engineCommitEpoch = useGameStore((store) => store.engineCommitEpoch);
  const gameState = useGameStore((store) => store.gameState);
  const adapter = useGameStore((store) => store.adapter);
  const playerNames = useMultiplayerStore((store) => store.playerNames);
  const [panelBinding, setPanelBinding] = useState<GameSessionBinding | null>(null);
  const [draft, setDraft] = useState<LifeCorrectionDraft | null>(null);
  const [feedback, setFeedback] = useState<CorrectionFeedback | null>(null);
  const [submission, setSubmission] = useState<SubmissionToken | null>(null);
  const [triggerPosition, setTriggerPosition] = useState<TriggerPosition | null>(null);
  const [panelPosition, setPanelPosition] = useState<PanelPosition | null>(null);
  const triggerSlotRef = useRef<HTMLSpanElement | null>(null);
  const triggerButtonRef = useRef<HTMLButtonElement | null>(null);
  const panelRef = useRef<HTMLDivElement | null>(null);
  const feedbackStatusRef = useRef<HTMLParagraphElement | null>(null);
  const submissionRef = useRef<SubmissionToken | null>(null);
  const requestIdRef = useRef(0);
  const wasOpenRef = useRef(false);
  const pendingPanelFocusRef = useRef(false);
  const restoreFocusAfterCloseRef = useRef(false);
  const restoreFocusBindingRef = useRef<GameSessionBinding | null>(null);

  const currentStore = useGameStore.getState();
  const isSupportedMode = gameMode === "ai" || gameMode === "local";
  const debugPlayers = gameState?.debug_permitted;
  const hasDebugPermission = !debugPlayers || debugPlayers.length === 0 || debugPlayers.includes(localPlayerId);
  const isSandboxGame = gameState?.debug_mode === true;
  const isEligible = sandboxFlagEnabled()
    && isSupportedMode
    && gameState !== null
    && adapter !== null
    && isSandboxGame
    && hasDebugPermission;
  const blockingOverlayActive = useBlockingOverlayActive(isEligible);

  const subscribedBinding: GameSessionBinding | null = adapter
    ? { adapter, gameId, gameSessionGeneration }
    : null;
  const storeBinding = bindingForStore(currentStore);
  const currentBinding = subscribedBinding !== null
    && storeBinding !== null
    && sameGameSession(subscribedBinding, storeBinding)
    ? subscribedBinding
    : null;
  const open = panelBinding !== null
    && currentBinding !== null
    && sameGameSession(panelBinding, currentBinding);
  const currentDraft = open
    && draft !== null
    && currentBinding !== null
    && sameGameSession(draft, currentBinding)
    ? draft
    : null;
  const currentFeedback = open
    && feedback !== null
    && currentBinding !== null
    && sameGameSession(feedback, currentBinding)
    ? feedback
    : null;
  const currentSubmission = submission !== null
    && currentBinding !== null
    && sameGameSession(submission.binding, currentBinding)
    ? submission
    : null;
  const submitting = currentSubmission !== null;
  const currentDraftIsValid = currentDraft !== null
    && engineCommitEpoch === currentStore.engineCommitEpoch
    && draftStillMatches(currentDraft, currentStore);
  const draftIsStale = currentDraft !== null && !currentDraftIsValid && !submitting;
  const nextLife = currentDraft ? Number(currentDraft.value) : Number.NaN;
  const validNewLife = currentDraft !== null
    && currentDraft.value.trim().length > 0
    && Number.isSafeInteger(nextLife)
    && nextLife >= -2_147_483_648
    && nextLife <= 2_147_483_647
    && nextLife !== currentDraft.initialLife;

  useEffect(() => {
    if (!blockingOverlayActive) return;
    requestIdRef.current += 1;
    setPanelBinding(null);
    setDraft(null);
    setFeedback(null);
  }, [blockingOverlayActive]);

  useLayoutEffect(() => {
    if (open) {
      if (!wasOpenRef.current) {
        wasOpenRef.current = true;
        const store = useGameStore.getState();
        pendingPanelFocusRef.current = isEligible
          && !blockingOverlayActive
          && !hasBlockingOverlay()
          && panelBinding !== null
          && sessionMatchesStore(panelBinding, store);
      }
      if (pendingPanelFocusRef.current) {
        const store = useGameStore.getState();
        if (
          !isEligible
          || blockingOverlayActive
          || hasBlockingOverlay()
          || !panelBinding
          || !sessionMatchesStore(panelBinding, store)
        ) {
          pendingPanelFocusRef.current = false;
        } else if (panelPosition) {
          pendingPanelFocusRef.current = false;
          panelRef.current?.focus();
        }
      }
      return;
    }

    wasOpenRef.current = false;
    pendingPanelFocusRef.current = false;
    if (!restoreFocusAfterCloseRef.current) return;
    restoreFocusAfterCloseRef.current = false;
    const focusBinding = restoreFocusBindingRef.current;
    restoreFocusBindingRef.current = null;
    const store = useGameStore.getState();
    if (
      isEligible
      && !blockingOverlayActive
      && !hasBlockingOverlay()
      && focusBinding !== null
      && sessionMatchesStore(focusBinding, store)
    ) {
      triggerButtonRef.current?.focus();
    }
  }, [open, isEligible, blockingOverlayActive, panelBinding, panelPosition]);

  useLayoutEffect(() => {
    if (!currentFeedback || currentDraft || !isEligible || blockingOverlayActive || hasBlockingOverlay()) return;
    if (!sessionMatchesStore(currentFeedback, useGameStore.getState())) return;
    feedbackStatusRef.current?.focus();
  }, [currentFeedback, currentDraft, isEligible, blockingOverlayActive]);

  useLayoutEffect(() => {
    if (!isEligible || blockingOverlayActive) return;

    const positionPanel = () => {
      const triggerSlot = triggerSlotRef.current;
      const panel = panelRef.current;
      if (!triggerSlot) {
        setTriggerPosition(null);
        setPanelPosition(null);
        return;
      }

      const triggerRect = triggerSlot.getBoundingClientRect();
      const nextTriggerPosition = {
        left: triggerRect.left,
        top: triggerRect.top,
        width: triggerRect.width,
        height: triggerRect.height,
      };
      setTriggerPosition((current) =>
        current?.left === nextTriggerPosition.left
          && current.top === nextTriggerPosition.top
          && current.width === nextTriggerPosition.width
          && current.height === nextTriggerPosition.height
          ? current
          : nextTriggerPosition,
      );
      if (!open || !panel) return;

      const panelRect = panel.getBoundingClientRect();
      const viewportPadding = 8;
      const gap = 8;
      const availableAbove = Math.max(0, triggerRect.top - gap - viewportPadding);
      const availableBelow = Math.max(0, window.innerHeight - triggerRect.bottom - gap - viewportPadding);
      const placeAbove = panelRect.height <= availableAbove || availableAbove >= availableBelow;
      const availableHeight = placeAbove ? availableAbove : availableBelow;
      const panelHeight = Math.min(panelRect.height, availableHeight);
      const left = Math.min(
        Math.max(viewportPadding, triggerRect.right - panelRect.width),
        Math.max(viewportPadding, window.innerWidth - panelRect.width - viewportPadding),
      );
      const top = placeAbove
        ? Math.max(viewportPadding, triggerRect.top - gap - panelHeight)
        : triggerRect.bottom + gap;

      setPanelPosition((current) =>
        current?.left === left && current.top === top && current.maxHeight === availableHeight
          ? current
          : { left, top, maxHeight: availableHeight },
      );
    };

    positionPanel();
    const triggerSlot = triggerSlotRef.current;
    const flexWidget = triggerSlot?.closest<HTMLElement>("[data-flex-zone]");
    const resizeObserver = typeof ResizeObserver === "undefined"
      ? null
      : new ResizeObserver(positionPanel);
    if (triggerSlot) resizeObserver?.observe(triggerSlot);
    if (panelRef.current) resizeObserver?.observe(panelRef.current);

    // DraggableWidget moves the HUD with Framer Motion transforms. Those
    // coordinates change without scroll/resize events, so follow its inline
    // motion style updates and keep the body portal attached to the HUD.
    const anchorObserver = flexWidget && typeof MutationObserver !== "undefined"
      ? new MutationObserver(positionPanel)
      : null;
    if (flexWidget && anchorObserver) {
      anchorObserver.observe(flexWidget, { attributes: true, attributeFilter: ["style"] });
    }

    window.addEventListener("resize", positionPanel);
    window.addEventListener("scroll", positionPanel, true);
    return () => {
      resizeObserver?.disconnect();
      anchorObserver?.disconnect();
      window.removeEventListener("resize", positionPanel);
      window.removeEventListener("scroll", positionPanel, true);
    };
  }, [isEligible, blockingOverlayActive, open, submitting, currentFeedback?.status]);

  if (!isEligible || blockingOverlayActive || !gameState) {
    return null;
  }

  const playerIdentity = (playerId: PlayerId) => {
    const name = playerId === localPlayerId
      ? t("sandboxLifeCorrection.you")
      : playerNames.get(playerId) ?? t("sandboxLifeCorrection.opponent");
    return t("sandboxLifeCorrection.player", { name, seat: playerId + 1 });
  };
  const currentFeedbackLife = currentFeedback?.status === "applied"
    ? gameState.players.find((player) => player.id === currentFeedback.targetPlayerId)?.life ?? null
    : null;
  const feedbackText = currentFeedback
    ? currentFeedback.status === "applied" && currentFeedbackLife !== null
      ? t("sandboxLifeCorrection.applied", {
        player: playerIdentity(currentFeedback.targetPlayerId),
        life: currentFeedbackLife,
      })
      : t(`sandboxLifeCorrection.${currentFeedback.status}`)
    : null;

  const closePanel = () => {
    restoreFocusAfterCloseRef.current = true;
    restoreFocusBindingRef.current = panelBinding;
    requestIdRef.current += 1;
    setPanelBinding(null);
    setDraft(null);
    setFeedback(null);
  };

  const isolateKeyboardFromGameShortcuts = (event: ReactKeyboardEvent<HTMLElement>) => {
    // This leaf owns all keydown interactions, including Escape and game-wide
    // shortcuts such as undo/tap-for-mana. Do not prevent native activation,
    // text editing, or focus navigation, and leave keyup to global listeners.
    event.stopPropagation();
    if (event.key === "Escape" && open) closePanel();
  };

  const openPanel = () => {
    const store = useGameStore.getState();
    const targetPlayerId = store.gameState?.players.some((player) => player.id === localPlayerId)
      ? localPlayerId
      : store.gameState?.players[0]?.id;
    if (targetPlayerId === undefined) return;
    const nextDraft = captureDraft(targetPlayerId, store);
    if (!nextDraft) return;

    requestIdRef.current += 1;
    setPanelBinding(nextDraft);
    setDraft(nextDraft);
    setFeedback(null);
  };

  const changeTarget = (value: string) => {
    const store = useGameStore.getState();
    if (!panelBinding || !sessionMatchesStore(panelBinding, store)) return;
    const targetPlayerId = Number(value);
    const nextDraft = captureDraft(targetPlayerId, store);
    if (!nextDraft) return;
    setDraft(nextDraft);
    setFeedback(null);
  };

  const submitCorrection = async () => {
    if (!currentDraft || !validNewLife) return;

    const before = useGameStore.getState();
    if (!draftStillMatches(currentDraft, before)) {
      if (sessionMatchesStore(currentDraft, before)) {
        setFeedback({
          ...currentDraft,
          status: "notApplied",
          targetPlayerId: currentDraft.targetPlayerId,
        });
      }
      return;
    }

    const binding = bindingForStore(before);
    if (!binding || !sameGameSession(currentDraft, binding)) return;
    const activeRequest = submissionRef.current;
    if (activeRequest && sameGameSession(activeRequest.binding, binding)) return;

    const submittedActor = getPlayerId();
    const token: SubmissionToken = { requestId: ++requestIdRef.current, binding };
    submissionRef.current = token;
    setSubmission(token);
    setFeedback(null);

    try {
      await dispatch({
        type: "Debug",
        data: {
          type: "SetLife",
          data: { player_id: currentDraft.targetPlayerId, life: nextLife },
        },
      });

      const after = useGameStore.getState();
      const sameSession = sessionMatchesStore(binding, after);
      const committedLife = after.gameState?.players.find((player) => player.id === currentDraft.targetPlayerId)?.life;
      const applied = sameSession
        && after.engineCommitEpoch > before.engineCommitEpoch
        && after.lastCommittedSeq > before.lastCommittedSeq
        && committedLife === nextLife
        && after.events !== before.events
        && confirmsCorrection(
          after.events,
          submittedActor,
          currentDraft.targetPlayerId,
          nextLife,
        );

      if (requestIdRef.current === token.requestId && sessionMatchesStore(binding, useGameStore.getState())) {
        setDraft(null);
        setFeedback({
          ...binding,
          status: applied ? "applied" : "notApplied",
          targetPlayerId: currentDraft.targetPlayerId,
        });
      }
    } catch {
      if (requestIdRef.current === token.requestId && sessionMatchesStore(binding, useGameStore.getState())) {
        setDraft(null);
        setFeedback({
          ...binding,
          status: "failed",
          targetPlayerId: currentDraft.targetPlayerId,
        });
      }
    } finally {
      if (submissionRef.current === token) {
        submissionRef.current = null;
        setSubmission(null);
      }
    }
  };

  return (
    <>
      <span
        ref={triggerSlotRef}
        data-sandbox-life-correction-anchor=""
        aria-hidden="true"
        className="inline-flex"
      >
        <button
          type="button"
          tabIndex={-1}
          className="invisible pointer-events-none min-h-11 touch-manipulation rounded-full bg-gray-800/80 px-3 text-[10px] font-medium text-amber-200"
        >
          {t("sandboxLifeCorrection.open")}
        </button>
      </span>
      {createPortal(
        <>
          <button
            data-sandbox-life-correction="trigger"
            ref={triggerButtonRef}
            type="button"
            aria-expanded={open}
            onKeyDown={isolateKeyboardFromGameShortcuts}
            onClick={open ? closePanel : openPanel}
            style={{
              left: triggerPosition?.left ?? 8,
              top: triggerPosition?.top ?? 8,
              width: triggerPosition?.width ?? 0,
              height: triggerPosition?.height ?? 0,
              visibility: triggerPosition ? "visible" : "hidden",
            }}
            className="fixed z-[130] min-h-11 touch-manipulation rounded-full bg-gray-800/80 px-3 text-[10px] font-medium text-amber-200 transition-colors hover:bg-gray-700/80"
          >
            {t("sandboxLifeCorrection.open")}
          </button>
          {open ? (
            <div
              data-sandbox-life-correction="panel"
              ref={panelRef}
              role="dialog"
              aria-label={t("sandboxLifeCorrection.title")}
              tabIndex={-1}
              onKeyDown={isolateKeyboardFromGameShortcuts}
              className="fixed z-[130] w-72 max-w-[calc(100vw-1rem)] overflow-y-auto rounded-lg border border-amber-700/50 bg-gray-950 p-3 text-xs text-gray-200 shadow-xl"
              style={{
                left: panelPosition?.left ?? 8,
                top: panelPosition?.top ?? 8,
                maxHeight: panelPosition && panelPosition.maxHeight > 0
                  ? panelPosition.maxHeight
                  : "calc(100dvh - 1rem)",
                visibility: panelPosition ? "visible" : "hidden",
              }}
            >
              <div className="mb-2 flex items-start justify-between gap-2">
                <h2 className="font-semibold text-amber-100">{t("sandboxLifeCorrection.title")}</h2>
                <button
                  type="button"
                  onClick={closePanel}
                  aria-label={submitting || currentFeedback ? t("actions.close", { ns: "common" }) : t("actions.cancel", { ns: "common" })}
                  className="-mr-1 -mt-1 min-h-11 min-w-11 touch-manipulation rounded px-2 text-gray-400 hover:bg-gray-800 hover:text-white"
                >
                  {submitting || currentFeedback ? t("actions.close", { ns: "common" }) : t("actions.cancel", { ns: "common" })}
                </button>
              </div>
              <p className="mb-2 text-gray-400">{t("sandboxLifeCorrection.description")}</p>
              <p className="mb-3 text-[11px] text-amber-200/80">{t("sandboxLifeCorrection.scopeNote")}</p>

              {currentFeedback && currentFeedback.status !== "applied" && gameState.players.some((player) => player.id === currentFeedback.targetPlayerId) ? (
                <div className="mb-2 flex items-center justify-between rounded bg-gray-900 px-2 py-1.5">
                  <span className="text-gray-400">{t("sandboxLifeCorrection.currentLife")}</span>
                  <LifeTotal playerId={currentFeedback.targetPlayerId} size="sm" hideLabel />
              </div>
              ) : null}
              {submitting ? (
                <p role="status" className="mb-2 text-amber-200">{t("sandboxLifeCorrection.submitting")}</p>
              ) : null}
              {currentDraft && currentDraftIsValid ? (
                <form
                  aria-label={t("sandboxLifeCorrection.title")}
                  onSubmit={(event) => {
                    event.preventDefault();
                    void submitCorrection();
                  }}
                  className="space-y-2"
                >
                  <label className="block space-y-1">
                    <span className="text-gray-300">{t("sandboxLifeCorrection.target")}</span>
                    <select
                      aria-label={t("sandboxLifeCorrection.target")}
                      value={currentDraft.targetPlayerId}
                      disabled={submitting}
                      onChange={(event) => changeTarget(event.currentTarget.value)}
                      className="min-h-11 w-full touch-manipulation rounded border border-gray-700 bg-gray-900 px-2 py-1 text-white"
                    >
                      {gameState.players.map((player) => (
                        <option key={player.id} value={player.id}>
                          {playerIdentity(player.id)}
                        </option>
                      ))}
                    </select>
                  </label>
                  <div className="flex items-center justify-between rounded bg-gray-900 px-2 py-1.5">
                    <span className="text-gray-400">{t("sandboxLifeCorrection.currentLife")}</span>
                    <LifeTotal playerId={currentDraft.targetPlayerId} size="sm" hideLabel />
                  </div>
                  <label className="block space-y-1">
                    <span className="text-gray-300">{t("sandboxLifeCorrection.newLife")}</span>
                    <input
                      type="number"
                      step="1"
                      min="-2147483648"
                      max="2147483647"
                      value={currentDraft.value}
                      disabled={submitting}
                      onChange={(event) => setDraft({ ...currentDraft, value: event.currentTarget.value })}
                      className="min-h-11 w-full touch-manipulation rounded border border-gray-700 bg-gray-900 px-2 py-1 text-white"
                    />
                  </label>
                  {currentFeedback ? (
                    <p role="status" className={currentFeedback.status === "applied" ? "text-emerald-300" : "text-amber-200"}>
                      {feedbackText}
                    </p>
                  ) : null}
                  <button
                    type="submit"
                    disabled={submitting || !validNewLife}
                    className="min-h-11 w-full touch-manipulation rounded bg-amber-700 px-2 py-1.5 font-medium text-white hover:bg-amber-600 disabled:cursor-not-allowed disabled:opacity-50"
                  >
                    {t("sandboxLifeCorrection.submit")}
                  </button>
                </form>
              ) : draftIsStale ? (
                <p role="alert" className="text-amber-200">{t("sandboxLifeCorrection.stale")}</p>
              ) : null}
              {currentFeedback && !currentDraft ? (
                <p
                  ref={feedbackStatusRef}
                  role="status"
                  tabIndex={-1}
                  className={currentFeedback.status === "applied" ? "text-emerald-300" : "text-amber-200"}
                >
                  {feedbackText}
                </p>
              ) : null}
            </div>
          ) : null}
        </>,
        document.body,
      )}
    </>
  );
}
