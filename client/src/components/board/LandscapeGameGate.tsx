import { type ReactNode, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { useGameViewportLock } from "../../hooks/useGameViewportLock.ts";

interface LandscapeGameGateProps {
  onExit: () => void;
}

interface LandscapeGameBoundaryProps extends LandscapeGameGateProps {
  children: ReactNode;
  requiresLandscape: boolean;
  sessionId: string;
}

/**
 * Defers a new mobile session until landscape is available. Once admitted, the
 * session remains mounted behind the gate if the device rotates back to
 * portrait, preserving network and engine continuity.
 */
export function LandscapeGameBoundary({
  children,
  onExit,
  requiresLandscape,
  sessionId,
}: LandscapeGameBoundaryProps) {
  const [admission, setAdmission] = useState(
    () => ({ sessionId, admitted: !requiresLandscape }),
  );

  useEffect(() => {
    setAdmission((current) => {
      if (current.sessionId !== sessionId) {
        return { sessionId, admitted: !requiresLandscape };
      }
      if (!requiresLandscape && !current.admitted) {
        return { sessionId, admitted: true };
      }
      return current;
    });
  }, [requiresLandscape, sessionId]);

  const admitted =
    admission.sessionId === sessionId
      ? admission.admitted
      : !requiresLandscape;

  if (!admitted) {
    return <LandscapeGameGate onExit={onExit} />;
  }

  return (
    <>
      {children}
      {requiresLandscape ? <LandscapeGameGate onExit={onExit} /> : null}
    </>
  );
}

export function LandscapeGameGate({ onExit }: LandscapeGameGateProps) {
  const { t } = useTranslation("game");
  useGameViewportLock();

  return (
    <div
      className="fixed inset-0 z-[200] flex touch-none items-center justify-center overflow-hidden bg-[#090b09] px-[calc(env(safe-area-inset-left)+1.5rem)] py-[calc(env(safe-area-inset-top)+1.5rem)] text-stone-100"
      role="dialog"
      aria-modal="true"
      aria-labelledby="landscape-game-gate-title"
      data-landscape-game-gate
    >
      <div className="pointer-events-none absolute inset-0 bg-[radial-gradient(circle_at_50%_35%,rgba(185,155,95,0.13),transparent_38%),linear-gradient(145deg,rgba(255,255,255,0.025),transparent_45%)]" />
      <div className="relative flex max-w-md flex-col items-center text-center">
        <div
          className="mb-5 flex h-20 w-20 items-center justify-center rounded-full border border-[#c7b27e]/35 bg-black/35 shadow-[0_16px_36px_rgba(0,0,0,0.45),inset_0_1px_0_rgba(255,245,210,0.12)]"
          aria-hidden
        >
          <svg
            viewBox="0 0 64 64"
            className="h-12 w-12 text-[#d4c18d]"
            fill="none"
            stroke="currentColor"
            strokeWidth="3"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <rect x="20" y="9" width="24" height="38" rx="4" />
            <path d="M12 27a22 22 0 0 0 36 20" />
            <path d="m12 27 1-9 8 4" />
            <path d="m48 47-1 9-8-4" />
          </svg>
        </div>
        <h1
          id="landscape-game-gate-title"
          className="font-display text-2xl font-semibold tracking-tight text-[#eee3c4]"
        >
          {t("landscapeGate.title")}
        </h1>
        <p className="mt-2 max-w-sm text-sm leading-6 text-stone-300/80">
          {t("landscapeGate.description")}
        </p>
        <button
          type="button"
          onClick={onExit}
          className="mt-6 min-h-11 rounded-[5px] border border-[#b9a36f]/30 bg-[#1a1c17] px-5 py-2.5 text-xs font-bold uppercase tracking-[0.14em] text-[#e6d8b6] shadow-[inset_0_1px_0_rgba(255,245,210,0.08),0_10px_24px_rgba(0,0,0,0.3)] transition hover:border-[#d8c28c]/50 hover:bg-[#22251e] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[#a4dcda]"
        >
          {t("landscapeGate.exit")}
        </button>
      </div>
    </div>
  );
}
