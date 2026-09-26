import { motion } from "framer-motion";

import type { ManaCost, ObjectId } from "../../adapter/types.ts";
import { TabletopCardFace } from "../tabletop3d/TabletopCardFace.tsx";

interface TabletopStackCardSurfaceProps {
  objectId: ObjectId;
  displayCost?: ManaCost;
  transitDuration?: number;
}

/**
 * The single visual surface for a card entering or resting on the stack.
 * Keeping the composed face, shadow, and terminal glow here prevents the cast
 * animation from handing off to a visibly different card treatment.
 */
export function TabletopStackCardSurface({
  objectId,
  displayCost,
  transitDuration,
}: TabletopStackCardSurfaceProps) {
  const inTransit = transitDuration !== undefined;

  return (
    <div
      className="relative h-full w-full drop-shadow-[0_18px_22px_rgba(0,0,0,0.52)]"
      data-tabletop-stack-card-surface
      data-stack-card-state={inTransit ? "transit" : "settled"}
    >
      <TabletopCardFace
        objectId={objectId}
        displayCost={displayCost}
        className="h-full w-full"
        style={{
          "--hand-card-w": "100%",
          "--hand-card-h": "100%",
        } as React.CSSProperties}
      />
      {inTransit ? (
        <motion.div
          aria-hidden
          className="pointer-events-none absolute inset-0 rounded-[4.4%/3.15%]"
          initial={{ boxShadow: "0 0 5px rgba(119,190,198,0.18)" }}
          animate={{
            boxShadow: [
              "0 0 5px rgba(119,190,198,0.18)",
              "0 0 22px rgba(119,190,198,0.46)",
              "0 0 8px rgba(216,207,178,0.24)",
            ],
          }}
          transition={{ duration: transitDuration, times: [0, 0.52, 1] }}
        />
      ) : (
        <div
          aria-hidden
          className="pointer-events-none absolute inset-0 rounded-[4.4%/3.15%] shadow-[0_0_8px_rgba(216,207,178,0.24)]"
        />
      )}
    </div>
  );
}
