import { Component, type ErrorInfo, type ReactNode } from "react";
import { useTranslation } from "react-i18next";

import { reportBoundaryError } from "../services/telemetryEvents";

/**
 * Full-screen fallback shown after an uncaught render error. Kept as a small
 * function component so it can use `useTranslation`; the class boundary itself
 * cannot. The only recovery affordance is a hard reload — the React tree below
 * the boundary is already unmounted and unrecoverable in place.
 */
function ErrorFallback() {
  const { t } = useTranslation("common");
  return (
    <div className="fixed inset-0 z-[200] flex flex-col items-center justify-center gap-4 bg-gray-950 p-6 text-center text-white">
      <h1 className="text-lg font-semibold text-slate-100">{t("errorBoundary.heading")}</h1>
      <p className="max-w-md text-sm text-slate-400">{t("errorBoundary.body")}</p>
      <button
        type="button"
        onClick={() => window.location.reload()}
        className="rounded-[14px] border border-white/10 bg-white/5 px-4 py-2 text-sm font-medium text-slate-100 transition hover:bg-white/10"
      >
        {t("errorBoundary.reload")}
      </button>
    </div>
  );
}

interface ErrorBoundaryProps {
  children: ReactNode;
}

interface ErrorBoundaryState {
  hasError: boolean;
}

/**
 * React error boundary for uncaught render errors (a real gap: only engine
 * panics were handled before). Reports each caught error to telemetry as a
 * `js_error` with `source: "boundary"` and renders a reload prompt so a broken
 * render doesn't leave the user staring at a blank or half-painted screen.
 */
export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { hasError: false };

  static getDerivedStateFromError(): ErrorBoundaryState {
    return { hasError: true };
  }

  componentDidCatch(error: Error, _info: ErrorInfo): void {
    reportBoundaryError(error);
  }

  render(): ReactNode {
    if (this.state.hasError) return <ErrorFallback />;
    return this.props.children;
  }
}
