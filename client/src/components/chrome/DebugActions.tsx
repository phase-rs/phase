import { useState } from "react";

import type { DebugAction } from "../../adapter/types";
import { useGameDispatch } from "../../hooks/useGameDispatch";
import { usePlayerId } from "../../hooks/usePlayerId";
import { useGameStore } from "../../stores/gameStore";
import { useUiStore } from "../../stores/uiStore";
import { StatusMessage } from "./debugFields";
import { DebugCreateActions } from "./DebugCreateActions";
import { DebugFlowActions } from "./DebugFlowActions";
import { DebugObjectActions } from "./DebugObjectActions";
import { DebugPlayerActions } from "./DebugPlayerActions";
import { GrantDebugPermissionPanel } from "./GrantDebugPermissionPanel";

type Category = "player" | "object" | "flow" | "create";

const TABS: readonly { key: Category; label: string }[] = [
  { key: "player", label: "Player" },
  { key: "object", label: "Object" },
  { key: "flow", label: "Flow" },
  { key: "create", label: "Create" },
] as const;

export function DebugActions() {
  const [activeTab, setActiveTab] = useState<Category>("player");
  const [status, setStatus] = useState<{ type: "success" | "error"; message: string } | null>(null);
  const dispatch = useGameDispatch();
  const debugInteractionMode = useUiStore((s) => s.debugInteractionMode);
  const toggleDebugInteractionMode = useUiStore((s) => s.toggleDebugInteractionMode);
  const localPlayerId = usePlayerId();
  // Single-player / AI / local games leave `debug_permitted` empty, in which
  // case `debug_mode` itself is the engine gate and the panel renders as
  // before. In a multiplayer sandbox the set is populated and only members
  // see the action tabs.
  const debugPermitted = useGameStore((s) => s.gameState?.debug_permitted);
  const allowDebug = useGameStore(
    (s) => s.gameState?.format_config?.allow_debug_actions === true,
  );
  const isHost = localPlayerId === 0;
  const hasPermission =
    !debugPermitted || debugPermitted.length === 0 || debugPermitted.includes(localPlayerId);

  const handleDispatch = async (action: DebugAction) => {
    setStatus(null);
    try {
      await dispatch({ type: "Debug", data: action });
      setStatus({ type: "success", message: `${action.type} applied` });
    } catch {
      setStatus({ type: "error", message: `${action.type} failed` });
    }
  };

  if (!hasPermission) {
    return (
      <div className="px-2 py-3 text-xs text-gray-500">
        Debug actions are disabled for this seat. The host can grant
        permission from their own Debug panel.
      </div>
    );
  }

  return (
    <div>
      {allowDebug && isHost && <GrantDebugPermissionPanel />}
      <div className="mb-1 flex items-center justify-between">
        <h3 className="font-mono text-xs font-bold uppercase tracking-wider text-gray-500">
          Debug Actions
        </h3>
        <button
          onClick={toggleDebugInteractionMode}
          className={
            "rounded-full border px-2 py-0.5 font-mono text-[10px] uppercase tracking-wider transition-colors " +
            (debugInteractionMode
              ? "border-amber-500/60 bg-amber-500/20 text-amber-300"
              : "border-gray-700 bg-transparent text-gray-600 hover:border-gray-600 hover:text-gray-500")
          }
        >
          {debugInteractionMode ? "Click Mode ON" : "Click Mode"}
        </button>
      </div>
      <div className="mb-2 flex flex-wrap gap-1">
        {TABS.map(({ key, label }) => {
          const active = activeTab === key;
          return (
            <button
              key={key}
              onClick={() => setActiveTab(key)}
              className={
                "rounded-full border px-2 py-0.5 font-mono text-[10px] uppercase tracking-wider transition-colors " +
                (active
                  ? "border-blue-500/60 bg-blue-500/20 text-blue-300"
                  : "border-gray-700 bg-transparent text-gray-600 hover:border-gray-600 hover:text-gray-500")
              }
            >
              {label}
            </button>
          );
        })}
      </div>
      <div>
        {activeTab === "player" && <DebugPlayerActions onDispatch={handleDispatch} />}
        {activeTab === "object" && <DebugObjectActions onDispatch={handleDispatch} />}
        {activeTab === "flow" && <DebugFlowActions onDispatch={handleDispatch} />}
        {activeTab === "create" && <DebugCreateActions onDispatch={handleDispatch} />}
      </div>
      {status && <StatusMessage status={status} />}
    </div>
  );
}
