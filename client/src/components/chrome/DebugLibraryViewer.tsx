import { useCallback, useMemo, useState, type CSSProperties } from "react";

import type { DebugAction, GameObject } from "../../adapter/types";
import { CardImage } from "../card/CardImage";
import { ModalPanelShell } from "../ui/ModalPanelShell";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps";
import { useGameDispatch } from "../../hooks/useGameDispatch";
import { useGameStore } from "../../stores/gameStore";
import { useUiStore } from "../../stores/uiStore";

/**
 * Debug-only library browser. Lists the given player's full library so a
 * specific card can be moved to any zone (battlefield, hand, …) without
 * scrubbing through a dropdown.
 *
 * The cards are shown in a STABLE RANDOMIZED order rather than their true
 * library order. The engine deliberately leaves the on-wire `library` Vec order
 * untouched (sandbox debug exposes card *names* but must not leak *draw order*,
 * per `visibility.rs`), so this view shuffles the display once per open. Moving
 * a card out simply removes it from its slot — the rest keep their positions.
 */
export function DebugLibraryViewer() {
  const viewer = useUiStore((s) => s.debugLibraryViewer);
  const close = useUiStore((s) => s.closeDebugLibraryViewer);

  if (!viewer) return null;

  return <DebugLibraryViewerInner playerId={viewer.playerId} onClose={close} />;
}

function DebugLibraryViewerInner({
  playerId,
  onClose,
}: {
  playerId: number;
  onClose: () => void;
}) {
  const objects = useGameStore((s) => s.gameState?.objects);
  const libraryIds = useGameStore((s) => s.gameState?.players[playerId]?.library);
  const openDebugContextMenu = useUiStore((s) => s.openDebugContextMenu);
  const dispatch = useGameDispatch();

  // One shuffle seed per open: keeps the order stable across re-renders (and
  // across card moves) so the grid doesn't reshuffle every time a card leaves.
  const [seed] = useState(() => Math.floor(Math.random() * 2 ** 31));

  const cards = useMemo(() => {
    if (!objects || !libraryIds) return [];
    const rank = (id: number) => {
      // Deterministic per (seed, id) pseudo-random key — a stable shuffle.
      const x = Math.sin((id + 1) * (seed + 1)) * 43758.5453;
      return x - Math.floor(x);
    };
    return libraryIds
      .map((id) => objects[id])
      .filter((obj): obj is GameObject => Boolean(obj))
      .sort((a, b) => rank(a.id) - rank(b.id));
  }, [objects, libraryIds, seed]);

  const move = useCallback(
    (objectId: number, toZone: "Battlefield" | "Hand") => {
      const action: DebugAction = {
        type: "MoveToZone",
        data: { object_id: objectId, to_zone: toZone },
      };
      void dispatch({ type: "Debug", data: action });
    },
    [dispatch],
  );

  return (
    <ModalPanelShell
      title={`Library — Player ${playerId} (${cards.length})`}
      subtitle="Debug: shown in randomized order. Click a card for all zones; use the buttons for quick moves."
      onClose={onClose}
      maxWidthClassName="max-w-6xl"
      bodyClassName="flex min-h-0 flex-col"
    >
      <div className="min-h-0 flex-1 overflow-y-auto px-3 pb-4 lg:px-6">
        {cards.length === 0 ? (
          <p className="py-8 text-center text-sm italic text-gray-600">
            Library is empty.
          </p>
        ) : (
          <div
            className="flex flex-wrap justify-center gap-3"
            style={
              {
                "--card-w": "clamp(100px, 9vw, 150px)",
                "--card-h": "clamp(140px, 12.6vw, 210px)",
              } as CSSProperties
            }
          >
            {cards.map((obj) => (
              <LibraryCard
                key={obj.id}
                obj={obj}
                onOpenMenu={(x, y) => openDebugContextMenu({ objectId: obj.id, x, y })}
                onMove={(zone) => move(obj.id, zone)}
              />
            ))}
          </div>
        )}
      </div>
    </ModalPanelShell>
  );
}

function LibraryCard({
  obj,
  onOpenMenu,
  onMove,
}: {
  obj: GameObject;
  onOpenMenu: (x: number, y: number) => void;
  onMove: (zone: "Battlefield" | "Hand") => void;
}) {
  const hoverProps = useInspectHoverProps();

  return (
    <div
      className="group relative shrink-0 cursor-pointer rounded-lg transition-transform hover:scale-[1.03] hover:ring-1 hover:ring-white/20"
      data-card-hover
      {...hoverProps(obj.id)}
      onClick={(e) => {
        e.stopPropagation();
        onOpenMenu(e.clientX, e.clientY);
      }}
    >
      <CardImage cardName={obj.name} size="normal" />
      {/* Quick-move buttons for the two most common debug destinations; the
          full zone list is one click away via the card's debug context menu. */}
      <div className="pointer-events-none absolute inset-x-0 bottom-0 flex justify-center gap-1 rounded-b-lg bg-black/60 p-1 opacity-0 transition-opacity group-hover:pointer-events-auto group-hover:opacity-100">
        <QuickMoveButton label="BF" title="Move to battlefield" onClick={() => onMove("Battlefield")} />
        <QuickMoveButton label="Hand" title="Move to hand" onClick={() => onMove("Hand")} />
      </div>
    </div>
  );
}

function QuickMoveButton({
  label,
  title,
  onClick,
}: {
  label: string;
  title: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      title={title}
      onClick={(e) => {
        e.stopPropagation();
        onClick();
      }}
      className="rounded bg-blue-600/80 px-2 py-0.5 font-mono text-[10px] uppercase tracking-wider text-white transition-colors hover:bg-blue-500"
    >
      {label}
    </button>
  );
}
