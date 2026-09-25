import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";

import { isMultiplayerGameLive } from "../pwa/multiplayerGuard";
import { isDesktopTauri } from "../services/platform";
import { useAppNotificationStore } from "../stores/appToastStore";

/** Payload-free shell event: a validated deep link is waiting to be taken. */
const DEEP_LINK_PENDING_EVENT = "deep-link-pending";

/**
 * Desktop shell only: take the `phase://` link the shell validated and go
 * there. The shell owns validation and the destination origin; this hook only
 * pulls the result, on mount and on each `deep-link-pending` event. It never
 * reads the plugin's raw `deep-link://new-url` broadcast.
 *
 * While a multiplayer game or draft pod is live the link is taken and
 * discarded with an app-wide notice: the arrival would not leave the game, so
 * the user re-clicks the link once it ends.
 */
export function useDesktopDeepLinks(): void {
  const navigate = useNavigate();
  const { t } = useTranslation("multiplayer");
  // Read through refs so a route change or language switch neither
  // re-subscribes nor re-takes (GameProvider precedent).
  const navigateRef = useRef(navigate);
  const tRef = useRef(t);
  navigateRef.current = navigate;
  tRef.current = t;

  useEffect(() => {
    if (!isDesktopTauri()) return;

    const deliver = async () => {
      // Older shells lack the command; a rejection means "no link".
      const href = await invoke<string | null>("take_pending_deep_link").catch(() => null);
      if (href === null) return;
      if (isMultiplayerGameLive()) {
        useAppNotificationStore.getState().showNotification({
          title: tRef.current("deepLink.gameInProgressTitle"),
          description: tRef.current("deepLink.gameInProgressBody"),
        });
        return;
      }
      const url = new URL(href);
      if (url.origin === location.origin) {
        // A new history entry, so the multiplayer page's arrival effect runs.
        navigateRef.current(url.pathname + url.search);
      } else {
        location.assign(url.href);
      }
    };

    // Subscribe before the mount take, so a link delivered in between is
    // either in the slot for that take or announced to the listener.
    const subscribed = listen(DEEP_LINK_PENDING_EVENT, () => void deliver()).catch(() => null);
    void subscribed.then(() => deliver());
    return () => {
      void subscribed.then((stop) => stop?.());
    };
  }, []);
}
