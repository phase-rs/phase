import { useUiStore } from "../../stores/uiStore.ts";

export function FullControlToggle() {
  const fullControl = useUiStore((s) => s.fullControl);
  const toggleFullControl = useUiStore((s) => s.toggleFullControl);

  return (
    <button
      onClick={toggleFullControl}
      className={`rounded px-2 py-0.5 text-[10px] font-semibold transition-colors lg:px-3 lg:py-1 lg:text-xs ${
        fullControl
          ? "bg-amber-600 text-white"
          : "bg-gray-700 text-gray-400 hover:bg-gray-600"
      }`}
    >
      Full Control: {fullControl ? "ON" : "OFF"}
    </button>
  );
}
