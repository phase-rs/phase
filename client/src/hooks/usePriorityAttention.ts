import { useEffect, useRef } from "react";

import {
  acknowledgeInput,
  escalateAttention,
  escalationDeadline,
  initialAttentionMachine,
  attentionCue,
  noteElsewhere,
  observeLocalDecision,
  stepAttention,
  type AttentionCue,
  type AttentionMachine,
  type AttentionObservation,
  type Presence,
} from "../game/priorityAttention";
import { useGameStore } from "../stores/gameStore";
import { usePriorityAttentionStore } from "../stores/priorityAttentionStore";
import { usePreferencesStore } from "../stores/preferencesStore";
import { useUiStore } from "../stores/uiStore";
import { useCanActForWaitingState } from "./usePlayerId";

/**
 * Read side of the priority-attention cues. Components that render a cue
 * (pill, pass button) call this; the controller below is the single writer.
 */
export function usePriorityAttention(): { cue: AttentionCue; chimeCount: number } {
  const cue = usePriorityAttentionStore((s) => s.cue);
  const chimeCount = usePriorityAttentionStore((s) => s.chimeCount);
  return { cue, chimeCount };
}

/**
 * CSS class for the visual cue, shared by every surface that pulses (status
 * pill, primary priority button) so their gating cannot drift. Empty unless the
 * announcement is pending, the viewer can act on it, and the visual pref is on.
 */
export function usePriorityAttentionClass(canAct: boolean): string {
  const { cue } = usePriorityAttention();
  const visual = usePreferencesStore((s) => s.priorityAlertVisual);
  if (!visual || !canAct) return "";
  switch (cue) {
    case "none":
      return "";
    case "announced":
      return "priority-attention-glow";
    case "reminded":
      return "priority-attention-glow priority-attention-glow-escalated";
  }
}

/** A hidden tab, or a window that lost focus (another monitor, another app), is
 *  not in front of the player. */
function currentPresence(): Presence {
  return document.hidden || !document.hasFocus() ? "elsewhere" : "attending";
}

/** Keys whose press alone says nothing about the player being at the game. */
const MODIFIER_KEYS: ReadonlySet<string> = new Set(["Shift", "Control", "Alt", "Meta", "AltGraph"]);

/**
 * Owns one presentation session: the machine, its reminder timer, and the
 * mirror into the shared store. Every change goes through `apply`, so
 * observations, user input, the timer and resets cannot publish inconsistently.
 */
function createAttentionDriver() {
  // `chimeCount` only ever rises for the page's lifetime: the chime hook reads a
  // rise as a chime, so a restart must not take the count back down.
  const startOver = () =>
    initialAttentionMachine(Date.now(), usePriorityAttentionStore.getState().chimeCount);
  let machine = startOver();
  let timer: { deadline: number; id: ReturnType<typeof setTimeout> } | null = null;

  const disarm = () => {
    if (timer) clearTimeout(timer.id);
    timer = null;
  };

  const publish = () => {
    const store = usePriorityAttentionStore.getState();
    const next = { cue: attentionCue(machine), chimeCount: machine.chimeCount };
    if (store.cue !== next.cue || store.chimeCount !== next.chimeCount) store.publish(next);
  };

  // One timer per reminder deadline: re-commits of the same window keep it,
  // a closed or downgraded window cancels it.
  const rearm = () => {
    const deadline = escalationDeadline(machine);
    if (timer?.deadline === deadline) return;
    disarm();
    if (deadline === null) return;
    timer = {
      deadline,
      id: setTimeout(() => {
        timer = null;
        apply(escalateAttention(machine));
      }, Math.max(0, deadline - Date.now())),
    };
  };

  const apply = (next: AttentionMachine) => {
    machine = next;
    publish();
    rearm();
  };

  return {
    observe: (observation: AttentionObservation) =>
      apply(stepAttention(machine, observation, Date.now(), currentPresence())),
    /** The player left (tab hidden, window unfocused): a window still pending
     *  is not being watched, so it announces and chimes now. */
    left: () => apply(noteElsewhere(machine, currentPresence())),
    /** Real input: answers a held window, or restarts the idle clock. */
    input: () => apply(acknowledgeInput(machine, Date.now())),
    /** A replaced game is a fresh session: nothing pending, no idle time carried over. */
    reset: () => apply(startOver()),
    /** Leaving the game screen must not leave a cue or timer pending. */
    dispose: () => {
      disarm();
      const store = usePriorityAttentionStore.getState();
      store.publish({ cue: "none", chimeCount: store.chimeCount });
    },
  };
}

/**
 * THE single writer of `usePriorityAttentionStore`. Mount exactly once per
 * game screen. Watches the engine-committed pair (`waitingFor`, with
 * `autoPassRecommended` from the same commit) and announces a meaningful
 * priority window only when it opens after the local player had been away for
 * a while. It chimes only for a player who is elsewhere, or at a reactive
 * window's reminder — see `stepAttention`. Reuses `shouldAutoPass`, the game
 * loop's own predicate, so "meaningful" is never re-derived on the frontend.
 */
export function usePriorityAttentionController(): void {
  const gameState = useGameStore((s) => s.gameState);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const autoPassRecommended = useGameStore((s) => s.autoPassRecommended);
  const sessionGeneration = useGameStore((s) => s.gameSessionGeneration);
  const fullControl = useUiStore((s) => s.fullControl);
  const canActNow = useCanActForWaitingState();

  const driverRef = useRef<ReturnType<typeof createAttentionDriver> | null>(null);
  const sessionRef = useRef(sessionGeneration);

  useEffect(() => {
    driverRef.current ??= createAttentionDriver();
    if (sessionRef.current !== sessionGeneration) {
      sessionRef.current = sessionGeneration;
      driverRef.current.reset();
    }
    driverRef.current.observe(
      observeLocalDecision(gameState, waitingFor, canActNow, fullControl, autoPassRecommended),
    );
  }, [sessionGeneration, gameState, waitingFor, autoPassRecommended, fullControl, canActNow]);

  // Real input says the player is here. Capture phase so a component that stops
  // propagation still counts; passive because nothing is prevented. A bare
  // modifier, a shortcut chord or a held-down key is the player leaving (Ctrl+Tab,
  // Alt+Tab, Cmd+Tab) or nothing at all, not presence. And a tab that goes
  // hidden, or a window that loses focus, is not being watched, whatever the
  // clock says. The focus check guards against `blur` firing for focus moving
  // into an embedded frame, which leaves the page in front of the player.
  useEffect(() => {
    const options = { capture: true, passive: true } as const;
    const pointer = () => driverRef.current?.input();
    const key = (event: KeyboardEvent) => {
      if (event.repeat || event.ctrlKey || event.metaKey || event.altKey) return;
      if (MODIFIER_KEYS.has(event.key)) return;
      driverRef.current?.input();
    };
    const left = () => driverRef.current?.left();
    document.addEventListener("pointerdown", pointer, options);
    document.addEventListener("keydown", key, options);
    document.addEventListener("visibilitychange", left);
    window.addEventListener("blur", left);
    return () => {
      document.removeEventListener("pointerdown", pointer, options);
      document.removeEventListener("keydown", key, options);
      document.removeEventListener("visibilitychange", left);
      window.removeEventListener("blur", left);
    };
  }, []);

  useEffect(
    () => () => {
      driverRef.current?.dispose();
      driverRef.current = null;
    },
    [],
  );
}
