import { useEffect, useRef } from "react";
import { useLocation } from "react-router";

import { coarseRoute } from "../../services/telemetryEvents";
import { trackEvent } from "../../services/telemetry";

/**
 * Emits a `route_view` telemetry event on each navigation, keyed by the coarse
 * route (`coarseRoute` strips ids/params, so a gameId never leaves the client).
 * Consecutive repeats of the same coarse route are deduped — a param-only change
 * within one area (e.g. two `/game/:id`s) is one bucket, not two views.
 *
 * Mounted once in `AppContent` inside the router but OUTSIDE the per-route
 * `DevStrict` wrappers, so StrictMode's dev double-render can't double-emit.
 * telemetryEvents.ts itself stays React-free; this is the one component that
 * bridges the router to it.
 */
export function RouteTelemetry(): null {
  const { pathname } = useLocation();
  const lastRoute = useRef<string | null>(null);

  useEffect(() => {
    const route = coarseRoute(pathname);
    if (route === lastRoute.current) return;
    lastRoute.current = route;
    trackEvent("route_view", { route });
  }, [pathname]);

  return null;
}
