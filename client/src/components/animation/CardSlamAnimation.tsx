import { CARD_SLAM_FLIGHT_MS } from "../../animation/types.ts";

/**
 * Elements with a slam currently in flight. A collapsed identical-permanent
 * group renders one representative card for the whole swarm, so several
 * DamageDealt events in the same combat step resolve to the *same* DOM node.
 * Without this guard each would start its own rAF loop fighting over the
 * element's `translate`/`scale`. We animate it once; callers fall back to a
 * floating number for the rest (so every hit still shows a number).
 */
const activeSlams = new WeakSet<HTMLElement>();

/**
 * Arena-style card slam: animates the ACTUAL card DOM element from its
 * battlefield position toward the target, impacts with jitter, then
 * slides back to its original position.
 *
 * Uses independent CSS `translate`/`scale` properties so the animation
 * composes on top of Framer Motion's `transform` (rotate, opacity, y)
 * without conflict.
 *
 * Returns `true` if the slam started (and will fire `onImpact`), or `false`
 * if the element is already mid-slam — letting the caller show a floating
 * number instead of dropping the hit entirely.
 */
export function applyCardSlam(
  element: HTMLElement,
  targetX: number,
  targetY: number,
  speedMultiplier: number,
  onImpact: () => void,
): boolean {
  if (activeSlams.has(element)) return false;
  activeSlams.add(element);

  const rect = element.getBoundingClientRect();
  const centerX = rect.x + rect.width / 2;
  const centerY = rect.y + rect.height / 2;
  const dx = targetX - centerX;
  const dy = targetY - centerY;

  const flightMs = CARD_SLAM_FLIGHT_MS * speedMultiplier;
  const jitterMs = 300 * speedMultiplier;
  const returnMs = 250 * speedMultiplier;
  const totalMs = flightMs + jitterMs + returnMs;
  const start = performance.now();
  let impactFired = false;

  // Elevate above other cards during animation
  const originalZ = element.style.zIndex;
  element.style.zIndex = "100";

  const frame = (now: number) => {
    const elapsed = now - start;

    if (elapsed >= totalMs) {
      element.style.translate = "";
      element.style.scale = "";
      element.style.zIndex = originalZ;
      activeSlams.delete(element);
      return;
    }

    if (elapsed < flightMs) {
      // Flight: quadratic ease-in toward target (accelerating lunge)
      const t = elapsed / flightMs;
      const eased = t * t;
      element.style.translate = `${dx * eased}px ${dy * eased}px`;
      element.style.scale = `${1 + 0.12 * eased}`;
    } else if (elapsed < flightMs + jitterMs) {
      // Impact + decaying jitter oscillation at target position
      if (!impactFired) {
        impactFired = true;
        onImpact();
      }
      const jt = (elapsed - flightMs) / jitterMs;
      const decay = 1 - jt;
      const osc = Math.sin(jt * Math.PI * 6) * 8 * decay;
      element.style.translate = `${dx + osc}px ${dy + osc * 0.5}px`;
      element.style.scale = `${1 + decay * 0.04}`;
    } else {
      // Return to original position: quadratic ease-out
      const rt = (elapsed - flightMs - jitterMs) / returnMs;
      const eased = 1 - (1 - rt) * (1 - rt);
      element.style.translate = `${dx * (1 - eased)}px ${dy * (1 - eased)}px`;
      element.style.scale = "";
    }

    requestAnimationFrame(frame);
  };

  requestAnimationFrame(frame);
  return true;
}

/** How long a struck card takes to rock back to rest, before pace. */
export const CARD_KNOCKBACK_MS = 800;

/**
 * The struck card's knockback: pushed along the blow (`dirX`, `dirY`) and
 * pressed into the table, rocking as it settles back to rest; a bigger hit
 * rocks it further. Uses the independent `translate`/`rotate`/`scale`
 * properties and shares the slam's busy set, so the two never fight over one
 * element. Returns `false` if the element is already busy.
 */
export function applyCardKnockback(
  element: HTMLElement,
  dirX: number,
  dirY: number,
  amount: number,
  speedMultiplier: number,
): boolean {
  if (activeSlams.has(element)) return false;
  activeSlams.add(element);

  const length = Math.hypot(dirX, dirY) || 1;
  const ux = dirX / length;
  const uy = dirY / length;
  // A blow from the left rocks the card clockwise first.
  const tiltDeg = (ux >= 0 ? 1 : -1) * Math.min(4 + 1.2 * amount, 11);
  const durationMs = CARD_KNOCKBACK_MS * speedMultiplier;
  const start = performance.now();

  const frame = (now: number) => {
    const elapsed = now - start;
    if (elapsed >= durationMs) {
      element.style.translate = "";
      element.style.rotate = "";
      element.style.scale = "";
      activeSlams.delete(element);
      return;
    }
    const k = elapsed / 1000 / speedMultiplier;
    const push = 9 * (1 - Math.exp(-k * 40)) * Math.exp(-k * 6);
    element.style.translate = `${ux * push}px ${uy * push}px`;
    element.style.rotate = `${tiltDeg * Math.exp(-k * 8) * Math.sin(k * 22)}deg`;
    element.style.scale = `${1 - 0.05 * (1 - Math.exp(-k * 50)) * Math.exp(-k * 9)}`;
    requestAnimationFrame(frame);
  };

  requestAnimationFrame(frame);
  return true;
}
