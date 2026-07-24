import { useEffect, useRef, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import {
  getNativeEngineProgress,
  type NativeEngineProgress,
  type NativeEngineProgressPhase,
  subscribeNativeEngineProgress,
  useNativeEngineProvisioning,
} from "../../services/nativeEngine";

const REVEAL_DELAY_MS = 400;
const DISMISS_DELAY_MS = 1_500;
const FAILURE_DISMISS_DELAY_MS = 5_000;

function phaseLabel(
  phase: NativeEngineProgressPhase,
  t: ReturnType<typeof useTranslation>["t"],
): string {
  switch (phase) {
    case "resolving":
      return t("nativeEngineProgress.resolving");
    case "downloading_binary":
      return t("nativeEngineProgress.downloadingBinary");
    case "verifying":
      return t("nativeEngineProgress.verifying");
    case "downloading_data":
      return t("nativeEngineProgress.downloadingData");
    case "spawning":
      return t("nativeEngineProgress.spawning");
    case "ready":
      return t("nativeEngineProgress.ready");
    case "failed":
      return t("nativeEngineProgress.failed");
  }
}

/** App-wide status surface for shell-managed native-server provisioning. */
export function NativeEngineProgressOverlay() {
  const { t } = useTranslation("common");
  const provisioning = useNativeEngineProvisioning();
  const [progress, setProgress] = useState<NativeEngineProgress | null>(null);
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;
    let receivedLiveProgress = false;

    void subscribeNativeEngineProgress((next) => {
      receivedLiveProgress = true;
      if (active) setProgress(next);
    }).then((registeredUnlisten) => {
      if (!active) {
        registeredUnlisten();
        return;
      }
      unlisten = registeredUnlisten;
      return getNativeEngineProgress().then((latest) => {
        if (active && !receivedLiveProgress && latest) setProgress(latest);
      });
    }).catch(() => {
      // The browser build has no Tauri event bridge to subscribe to.
    });

    return () => {
      active = false;
      unlisten?.();
    };
  }, []);

  // A run's phases belong to that run: a previous attempt's terminal label must
  // not caption the next one before its first event lands. Seeded with the
  // mount-time value so only a run that starts while we are mounted clears the
  // label. Note the replayed phase above is label-only — visibility follows the
  // call, so a webview that reloads mid-run shows nothing until the next call.
  const previousProvisioning = useRef(provisioning);
  useEffect(() => {
    if (provisioning && !previousProvisioning.current) setProgress(null);
    previousProvisioning.current = provisioning;
  }, [provisioning]);

  // Announce provisioning only once the shell has been working long enough to
  // be worth interrupting for. Reusing an already-running engine answers in
  // milliseconds, and a full-screen blink on every game start reads as a bug.
  useEffect(() => {
    if (!provisioning) return;

    const timeout = window.setTimeout(() => setVisible(true), REVEAL_DELAY_MS);
    return () => window.clearTimeout(timeout);
  }, [provisioning]);

  // The overlay then lives for as long as the shell is working, plus a beat to
  // read the outcome. Deliberately NOT driven by receiving a terminal phase —
  // an older shell fails without emitting one, which pinned this overlay over
  // the running game for the rest of the session.
  useEffect(() => {
    if (provisioning || !visible) return;

    const timeout = window.setTimeout(
      () => setVisible(false),
      progress?.phase === "failed" ? FAILURE_DISMISS_DELAY_MS : DISMISS_DELAY_MS,
    );
    return () => window.clearTimeout(timeout);
  }, [provisioning, visible, progress]);

  return (
    <AnimatePresence>
      {visible && (
        <motion.div
          className="pointer-events-none fixed inset-0 z-[80] flex items-center justify-center bg-black/60 p-4 backdrop-blur-sm"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          role="status"
          aria-live="polite"
          aria-atomic="true"
          aria-busy={provisioning}
        >
          <motion.div
            className="w-full max-w-sm rounded-2xl border border-cyan-300/25 bg-slate-950/95 p-6 text-center shadow-[0_24px_80px_rgba(0,0,0,0.6)] ring-1 ring-white/10"
            initial={{ opacity: 0, scale: 0.96, y: 12 }}
            animate={{ opacity: 1, scale: 1, y: 0 }}
            exit={{ opacity: 0, scale: 0.96, y: 12 }}
          >
            <div className="mx-auto flex h-12 w-12 items-center justify-center rounded-full bg-cyan-400/10 ring-1 ring-cyan-300/30">
              <div className="h-6 w-6 animate-spin rounded-full border-2 border-cyan-200/25 border-t-cyan-300" />
            </div>
            <p className="mt-4 text-base font-semibold text-white">
              {t("nativeEngineProgress.title")}
            </p>
            <p className="mt-2 text-sm text-cyan-100">
              {phaseLabel(progress?.phase ?? "resolving", t)}
            </p>
            {progress?.detail && (
              <p className="mt-3 break-all font-mono text-xs text-slate-400">
                {progress.detail}
              </p>
            )}
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>
  );
}
