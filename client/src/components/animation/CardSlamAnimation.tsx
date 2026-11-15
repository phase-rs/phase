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
 * battlefield position toward the target, recoils from impact, then
 * slides back to its original position.
 *
 * Uses independent CSS `translate`/`scale` properties so the animation
 * composes on top of Framer Motion's `transform` (rotate, opacity, y)
 * without conflict.
 *
 * `start` is when the slam begins on the frame clock (`performance.now()`),
 * so an effect given the same start lands on the same frame.
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
  start: number,
): boolean {
  if (activeSlams.has(element)) return false;
  activeSlams.add(element);

  const rect = element.getBoundingClientRect();
  const centerX = rect.x + rect.width / 2;
  const centerY = rect.y + rect.height / 2;
  const dx = targetX - centerX;
  const dy = targetY - centerY;

  const flightMs = CARD_SLAM_FLIGHT_MS * speedMultiplier;
  const recoilMs = 80 * speedMultiplier;
  const returnMs = 220 * speedMultiplier;
  const totalMs = flightMs + recoilMs + returnMs;
  const distance = Math.hypot(dx, dy);
  const pullback = Math.min(16, distance * 0.08);
  const ux = distance > 0 ? dx / distance : 0;
  const uy = distance > 0 ? dy / distance : 0;
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
      // Brief anticipation, then a fast lunge. Keep the shared impact clock.
      const t = elapsed / flightMs;
      if (t < 0.3) {
        const windup = Math.sin((t / 0.3) * Math.PI / 2);
        element.style.translate = `${-ux * pullback * windup}px ${-uy * pullback * windup}px`;
        element.style.scale = `${1 + 0.06 * windup}`;
      } else {
        const p = (t - 0.3) / 0.7;
        const eased = p * p;
        element.style.translate = `${-ux * pullback + (dx + ux * pullback) * eased}px ${-uy * pullback + (dy + uy * pullback) * eased}px`;
        element.style.scale = `${1.06 + 0.04 * Math.sin(p * Math.PI) - 0.06 * eased}`;
      }
    } else if (elapsed < flightMs + recoilMs) {
      // One short recoil along the strike, rather than shaking in place.
      if (!impactFired) {
        impactFired = true;
        onImpact();
      }
      const t = (elapsed - flightMs) / recoilMs;
      const recoil = 1 - 0.08 * (1 - (1 - t) ** 3);
      element.style.translate = `${dx * recoil}px ${dy * recoil}px`;
      element.style.scale = `${1 - 0.035 * Math.sin(t * Math.PI)}`;
    } else {
      // Return to original position: quadratic ease-out
      const rt = (elapsed - flightMs - recoilMs) / returnMs;
      const eased = 1 - (1 - rt) * (1 - rt);
      element.style.translate = `${dx * 0.92 * (1 - eased)}px ${dy * 0.92 * (1 - eased)}px`;
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
