import { useUiStore } from "../../stores/uiStore.ts";

export function FullControlToggle() {
  const fullControl = useUiStore((s) => s.fullControl);
  const toggleFullControl = useUiStore((s) => s.toggleFullControl);

  return (
    <button
      onClick={toggleFullControl}
      className={`rounded px-3 py-1 text-xs font-semibold transition-colors ${
        fullControl
          ? "bg-amber-600 text-white"
          : "bg-gray-700 text-gray-400 hover:bg-gray-600"
      }`}
    >
      Full Control: {fullControl ? "ON" : "OFF"}
    </button>
  );
}
