import { useIsPresent } from "framer-motion";
import { useState } from "react";

import type { ObjectId } from "../adapter/types.ts";
import { useAnimationStore } from "../stores/animationStore.ts";

/** Whether a card surface must not paint because a card flight is presenting
 *  its object (see `animationStore.flightVeiledObjectIds`), or because the
 *  surface began its exit while one was. A surface that mounts with this true
 *  skips its entrance: framer reads `initial` only at mount, so gating
 *  `initial` on this value suppresses the entrance for a veiled mount only. */
export function useFlightVeil(objectId: ObjectId): boolean {
  const veiled = useAnimationStore((s) => s.flightVeiledObjectIds.has(objectId));
  const isPresent = useIsPresent();
  // A surface leaving while veiled stays hidden for its whole exit animation,
  // even if the flight lands first; otherwise it would flash back into view.
  const [exitLatched, setExitLatched] = useState(false);
  if (!isPresent && veiled && !exitLatched) setExitLatched(true);
  return veiled || exitLatched;
}
