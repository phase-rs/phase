import { AnimatePresence, motion } from "framer-motion";

import { CardImage } from "../card/CardImage.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";

export function PlayerHand() {
  const player = useGameStore((s) => s.gameState?.players[0]);
  const objects = useGameStore((s) => s.gameState?.objects);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);
  const inspectObject = useUiStore((s) => s.inspectObject);

  if (!player || !objects) return null;

  const handObjects = player.hand
    .map((id) => objects[id])
    .filter(Boolean);

  const hasPriority =
    waitingFor?.type === "Priority" && waitingFor.data.player === 0;

  const handleCardClick = (objectId: number, _cardName: string, coreTypes: string[]) => {
    if (!hasPriority) return;

    if (coreTypes.includes("Land")) {
      dispatch({ type: "PlayLand", data: { card_id: objects[objectId].card_id } });
    } else {
      dispatch({ type: "CastSpell", data: { card_id: objects[objectId].card_id, targets: [] } });
    }
  };

  return (
    <div className="flex items-end justify-center gap-[-8px] border-t border-gray-800 bg-gray-900/80 px-4 py-2">
      <AnimatePresence>
        {handObjects.map((obj) => (
          <motion.div
            key={obj.id}
            layout
            initial={{ opacity: 0, y: 30 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0, y: 30 }}
            whileHover={{ y: -20 }}
            transition={{ duration: 0.2 }}
            className={`relative cursor-pointer ${
              hasPriority
                ? "shadow-[0_0_8px_2px_rgba(255,255,255,0.6)]"
                : ""
            } rounded-lg`}
            style={{ marginLeft: "-8px", marginRight: "-8px" }}
            onClick={() =>
              handleCardClick(obj.id, obj.name, obj.card_types.core_types)
            }
            onMouseEnter={() => inspectObject(obj.id)}
            onMouseLeave={() => inspectObject(null)}
          >
            <CardImage cardName={obj.name} size="small" />
          </motion.div>
        ))}
      </AnimatePresence>
    </div>
  );
}
