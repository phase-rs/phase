import type { GameAction, ManaCost, WaitingFor } from "../../adapter/types.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { ManaCostSymbols } from "../mana/ManaCostSymbols.tsx";
import { DialogShell } from "./DialogShell.tsx";

type MiracleReveal = Extract<WaitingFor, { type: "MiracleReveal" }>;
type MiracleCastOffer = Extract<WaitingFor, { type: "MiracleCastOffer" }>;
type MadnessCastOffer = Extract<WaitingFor, { type: "MadnessCastOffer" }>;

export function MiracleRevealModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  if (!canActForWaitingState) return null;

  if (waitingFor?.type === "MiracleReveal") {
    const data = waitingFor.data as MiracleReveal["data"];
    return (
      <MiracleRevealContent
        objectId={data.object_id}
        cost={data.cost}
        dispatch={dispatch}
        phase="reveal"
      />
    );
  }

  if (waitingFor?.type === "MiracleCastOffer") {
    const data = waitingFor.data as MiracleCastOffer["data"];
    return (
      <MiracleRevealContent
        objectId={data.object_id}
        cost={data.cost}
        dispatch={dispatch}
        phase="cast"
      />
    );
  }

  if (waitingFor?.type === "MadnessCastOffer") {
    const data = waitingFor.data as MadnessCastOffer["data"];
    return (
      <MiracleRevealContent
        objectId={data.object_id}
        cost={data.cost}
        dispatch={dispatch}
        phase="madness"
      />
    );
  }

  return null;
}

function MiracleRevealContent({
  objectId,
  cost,
  dispatch,
  phase,
}: {
  objectId: number;
  cost: ManaCost;
  dispatch: (action: GameAction) => Promise<unknown>;
  phase: "reveal" | "cast" | "madness";
}) {
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);

  if (!obj) return null;

  const cardName = obj.name;
  const cardId = obj.card_id;

  const isReveal = phase === "reveal";
  const isMadness = phase === "madness";
  const keywordLabel = isMadness ? "Madness" : "Miracle";
  const castAction: GameAction = isMadness
    ? {
        type: "CastSpellAsMadness",
        data: { object_id: objectId, card_id: cardId },
      }
    : {
        type: "CastSpellAsMiracle",
        data: { object_id: objectId, card_id: cardId },
      };

  return (
    <DialogShell
      eyebrow={keywordLabel}
      eyebrowClassName="text-amber-300/80"
      title={isReveal ? `Reveal ${cardName}?` : `Cast ${cardName}?`}
      subtitle={
        isReveal
          ? "You may reveal this card to cast it for its miracle cost."
          : `You may cast this card for its ${keywordLabel.toLowerCase()} cost.`
      }
    >
      <div className="flex flex-col gap-2 px-3 py-3 lg:px-5 lg:py-5">
        <button
          onClick={() => dispatch(castAction)}
          className="rounded-[16px] border border-amber-400/20 bg-amber-400/10 px-4 py-3 text-left transition hover:bg-amber-400/20 hover:ring-1 hover:ring-amber-400/40"
        >
          <span className="font-semibold text-white">
            {isReveal ? "Reveal" : "Cast"}
          </span>
          <span className="ml-2">
            <ManaCostSymbols cost={cost} />
          </span>
        </button>
        <button
          onClick={() =>
            dispatch({
              type: "DecideOptionalEffect",
              data: { accept: false },
            })
          }
          className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-white/20"
        >
          <span className="font-semibold text-white">Decline</span>
        </button>
      </div>
    </DialogShell>
  );
}
