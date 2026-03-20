import { AnimatePresence, motion } from "framer-motion";

import type { GameAction, WaitingFor } from "../../adapter/types.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";

type AdventureCastChoice = Extract<WaitingFor, { type: "AdventureCastChoice" }>;

export function AdventureCastModal() {
  const playerId = usePlayerId();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  if (waitingFor?.type !== "AdventureCastChoice") return null;
  if (waitingFor.data.player !== playerId) return null;

  const data = waitingFor.data as AdventureCastChoice["data"];

  return <AdventureCastContent objectId={data.object_id} dispatch={dispatch} />;
}

function AdventureCastContent({
  objectId,
  dispatch,
}: {
  objectId: number;
  dispatch: (action: GameAction) => Promise<unknown>;
}) {
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);

  if (!obj) return null;

  const creatureName = obj.name;
  const adventureName = obj.back_face?.name ?? "Adventure";

  return (
    <AnimatePresence>
      <motion.div
        className="fixed inset-0 z-50 flex items-center justify-center px-3 py-4 sm:px-4 sm:py-6"
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
        transition={{ duration: 0.2 }}
      >
        <div className="absolute inset-0 bg-black/60" />

        <motion.div
          className="relative z-10 w-full max-w-sm overflow-hidden rounded-[24px] border border-white/10 bg-[#0b1020]/96 shadow-[0_28px_80px_rgba(0,0,0,0.42)] backdrop-blur-md"
          initial={{ scale: 0.95, opacity: 0, y: 10 }}
          animate={{ scale: 1, opacity: 1, y: 0 }}
          exit={{ scale: 0.95, opacity: 0, y: 10 }}
          transition={{ duration: 0.2, ease: "easeOut" }}
        >
          <div className="border-b border-white/10 px-4 py-4 sm:px-5 sm:py-5">
            <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">
              Adventure
            </div>
            <h2 className="mt-1 text-lg font-semibold text-white sm:text-xl">Choose a Face</h2>
            <p className="mt-1 text-xs text-slate-400 sm:text-sm">
              Cast as the creature or as the Adventure spell.
            </p>
          </div>
          <div className="flex flex-col gap-2 px-4 py-4 sm:px-5 sm:py-5">
            <button
              onClick={() =>
                dispatch({ type: "ChooseAdventureFace", data: { creature: true } })
              }
              className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/30"
            >
              <span className="font-semibold text-white">Cast {creatureName}</span>
              <span className="ml-2 text-xs text-slate-400">(Creature)</span>
            </button>
            <button
              onClick={() =>
                dispatch({ type: "ChooseAdventureFace", data: { creature: false } })
              }
              className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-amber-400/30"
            >
              <span className="font-semibold text-white">Cast {adventureName}</span>
              <span className="ml-2 text-xs text-slate-400">(Adventure)</span>
            </button>
          </div>
        </motion.div>
      </motion.div>
    </AnimatePresence>
  );
}
