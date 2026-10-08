import { useEffect } from "react";
import { useNavigate } from "react-router";

import { useEffectiveOffline } from "../stores/connectivityStore";
import { useMultiplayerStore } from "../stores/multiplayerStore";

/**
 * Watches for a pending game route set by the hosting WebSocket's GameStarted
 * handler and navigates to it. This is the only place React Router interacts
 * with the hosting lifecycle — the store itself stays router-free.
 */
export function useHostingSession(): void {
  const effectiveOffline = useEffectiveOffline();
  const pendingGameRoute = useMultiplayerStore((s) => s.pendingGameRoute);
  const clearPendingGameRoute = useMultiplayerStore((s) => s.clearPendingGameRoute);
  const resumeServerHosting = useMultiplayerStore((s) => s.resumeServerHosting);
  const navigate = useNavigate();

  useEffect(() => {
    if (effectiveOffline) return;
    resumeServerHosting();
  }, [effectiveOffline, resumeServerHosting]);

  useEffect(() => {
    if (pendingGameRoute) {
      navigate(pendingGameRoute);
      clearPendingGameRoute();
    }
  }, [pendingGameRoute, navigate, clearPendingGameRoute]);
}
