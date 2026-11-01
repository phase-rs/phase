import type { GameEvent, PhysicalTell } from "../adapter/types";
import { type UnFxKind, useUnActionFxStore } from "../stores/unActionFxStore";

const PHYSICAL_FX: Record<PhysicalTell, UnFxKind> = {
  TouchFace: "touchFace",
  TouchTable: "touchTable",
  Laugh: "laugh",
  FlickCards: "flickCards",
};

/**
 * Surface Un-card gestures and spoken phrases out-of-band (UnActionOverlay),
 * the same way dice rolls bypass the animation queue: they are one-shot gags
 * that must play for every seat the moment the engine reports them, without
 * holding up game animations. The engine events are marked NON_VISUAL so
 * `normalizeEvents` skips them.
 */
export function flashUnActionEvents(events: GameEvent[]): void {
  const push = useUnActionFxStore.getState().push;
  for (const event of events) {
    switch (event.type) {
      case "UnTellPerformed": {
        const { player, tell } = event.data;
        if (tell.type === "Physical") {
          push({ kind: PHYSICAL_FX[tell.data], playerId: player });
        } else {
          push({ kind: "saysWord", playerId: player, text: tell.data.text });
        }
        break;
      }
      case "PhraseSaid":
        push({ kind: "phrase", playerId: event.data.player, text: event.data.phrase });
        break;
      case "GotchaSaid":
        push({ kind: "gotcha", playerId: event.data.player });
        break;
      default:
        break;
    }
  }
}
