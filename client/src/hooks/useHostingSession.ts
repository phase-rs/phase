import { useEffect } from "react";
import { useNavigate } from "react-router";

import { useMultiplayerStore } from "../stores/multiplayerStore";

/**
 * Watches for a pending game route set by the hosting WebSocket's GameStarted
 * handler and navigates to it. This is the only place React Router interacts
 * with the hosting lifecycle — the store itself stays router-free.
 */
export function useHostingSession(): void {
  const pendingGameRoute = useMultiplayerStore((s) => s.pendingGameRoute);
  const clearPendingGameRoute = useMultiplayerStore((s) => s.clearPendingGameRoute);
  const resumeServerHosting = useMultiplayerStore((s) => s.resumeServerHosting);
  const navigate = useNavigate();

  useEffect(() => {
    resumeServerHosting();
  }, [resumeServerHosting]);

  useEffect(() => {
    if (pendingGameRoute) {
      navigate(pendingGameRoute);
      clearPendingGameRoute();
    }
  }, [pendingGameRoute, navigate, clearPendingGameRoute]);
}
