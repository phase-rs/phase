import { GotchaButton } from "./GotchaButton.tsx";
import { UnActionOverlay } from "./UnActionOverlay.tsx";
import { UnActionsMenu } from "./UnActionsMenu.tsx";

/**
 * Everything the optional Un-card actions mode puts on the game screen: the
 * gesture menu, the timed Gotcha! button, and the gag animations. Each part
 * renders nothing unless the engine says it applies. The preference itself
 * reaches the engine through `useGameplayPreferencesSync`.
 */
export function UnActionsHost() {
  return (
    <>
      <UnActionOverlay />
      <UnActionsMenu />
      <GotchaButton />
    </>
  );
}
