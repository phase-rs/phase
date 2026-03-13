import { useCallback, useState } from "react";

type UpdateStatus = "idle" | "checking" | "up-to-date" | "error";

interface BuildBadgeProps {
  className?: string;
  inline?: boolean;
}

export function BuildBadge({ className = "", inline = false }: BuildBadgeProps = {}) {
  const [status, setStatus] = useState<UpdateStatus>("idle");

  const checkForUpdates = useCallback(async () => {
    if (status === "checking") return;
    setStatus("checking");
    try {
      const reg = await navigator.serviceWorker?.getRegistration();
      if (reg) {
        await reg.update();
        setStatus("up-to-date");
      } else {
        setStatus("up-to-date");
      }
      setTimeout(() => setStatus("idle"), 3000);
    } catch {
      setStatus("error");
      setTimeout(() => setStatus("idle"), 3000);
    }
  }, [status]);

  return (
    <div
      className={inline ? className : `fixed left-2 bottom-[calc(env(safe-area-inset-bottom)+0.5rem)] z-20 ${className}`.trim()}
    >
      <div className="flex items-center gap-1 rounded-full border border-white/10 bg-black/18 px-2.5 py-1.5 text-[10px] text-slate-400 shadow-lg shadow-black/30 backdrop-blur-md">
        <span>v{__APP_VERSION__}</span>
        <span className="text-slate-600">{__BUILD_HASH__}</span>
        <button
          type="button"
          onClick={checkForUpdates}
          className={`ml-0.5 text-slate-500 hover:text-white transition-colors cursor-pointer ${status === "checking" ? "animate-spin" : ""}`}
          aria-label="Check for updates"
          title="Check for updates"
        >
          ↻
        </button>
        {status === "checking" && <span className="text-cyan-300">checking…</span>}
        {status === "up-to-date" && <span className="text-emerald-300">up to date</span>}
        {status === "error" && <span className="text-rose-300">check failed</span>}
      </div>
    </div>
  );
}
