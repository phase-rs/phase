import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { CARD_SLAM_FLIGHT_MS } from "../../../animation/types.ts";
import { applyCardKnockback, applyCardSlam, CARD_KNOCKBACK_MS } from "../CardSlamAnimation.tsx";

const FRAME_MS = 16;

function card() {
  const el = document.createElement("div");
  el.getBoundingClientRect = () => new DOMRect(0, 0, 63, 88);
  document.body.appendChild(el);
  return el;
}

/** The pixel offset `translate` holds, as [x, y]. */
function offset(el: HTMLElement): [number, number] {
  const [x = "0px", y = "0px"] = el.style.translate.split(" ");
  return [Number.parseFloat(x), Number.parseFloat(y)];
}

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["requestAnimationFrame", "cancelAnimationFrame", "performance"] });
});

afterEach(() => {
  vi.useRealTimers();
  document.body.replaceChildren();
});

describe("applyCardSlam", () => {
  it("winds back before lunging, lands on the shared impact clock, and returns to rest", () => {
    const el = card();
    el.style.zIndex = "50";
    const impact = vi.fn();
    applyCardSlam(el, 231.5, 44, 1, impact, performance.now());

    vi.advanceTimersByTime(3 * FRAME_MS);
    expect(offset(el)[0]).toBeLessThan(0);
    expect(impact).not.toHaveBeenCalled();
    vi.advanceTimersByTime(CARD_SLAM_FLIGHT_MS - 3 * FRAME_MS);
    expect(impact).not.toHaveBeenCalled();
    vi.advanceTimersByTime(FRAME_MS);
    expect(impact).toHaveBeenCalledOnce();
    expect(offset(el)[0]).toBeGreaterThan(180);
    vi.advanceTimersByTime(4 * FRAME_MS);
    expect(offset(el)[0]).toBeLessThan(195);
    vi.advanceTimersByTime(300);
    expect(impact).toHaveBeenCalledOnce();
    expect([el.style.translate, el.style.scale, el.style.zIndex]).toEqual(["", "", "50"]);
    expect(applyCardSlam(el, 231.5, 44, 1, impact, performance.now())).toBe(true);
    vi.advanceTimersByTime(600);
  });

  it("cleans up an expired slam without replaying its damage feedback", () => {
    const frames: FrameRequestCallback[] = [];
    const raf = vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
      frames.push(callback);
      return frames.length;
    });
    const el = card();
    const impact = vi.fn();
    const start = performance.now();
    applyCardSlam(el, 400, 0, 1, impact, start);
    frames.shift()!(start + 1000);
    expect(impact).not.toHaveBeenCalled();
    expect(el.style.translate).toBe("");
    raf.mockRestore();
  });
});

describe("applyCardKnockback", () => {
  it("V12-3: the struck card is pushed along the blow, rocks, and comes back to rest with its styles cleared", () => {
    const el = card();
    expect(applyCardKnockback(el, 30, 0, 3, 1)).toBe(true);

    vi.advanceTimersByTime(4 * FRAME_MS);
    const [x, y] = offset(el);
    expect(x).toBeGreaterThan(1);
    expect(Math.abs(y)).toBeLessThan(0.01);
    expect(el.style.rotate).not.toBe("");
    expect(Number.parseFloat(el.style.scale)).toBeLessThan(1);

    vi.advanceTimersByTime(CARD_KNOCKBACK_MS);
    expect([el.style.translate, el.style.rotate, el.style.scale]).toEqual(["", "", ""]);
  });

  it("V12-3: a card mid-slam is not knocked back, and a knocked-back card does not slam until it rests", () => {
    const slamming = card();
    expect(applyCardSlam(slamming, 400, 0, 1, () => {}, performance.now())).toBe(true);
    expect(applyCardKnockback(slamming, 1, 0, 3, 1)).toBe(false);

    const struck = card();
    expect(applyCardKnockback(struck, 1, 0, 3, 1)).toBe(true);
    expect(applyCardSlam(struck, 400, 0, 1, () => {}, performance.now())).toBe(false);
    vi.advanceTimersByTime(CARD_KNOCKBACK_MS + FRAME_MS);
    expect(applyCardSlam(struck, 400, 0, 1, () => {}, performance.now())).toBe(true);
    vi.advanceTimersByTime(CARD_SLAM_FLIGHT_MS + 1000);
  });

  it("V12-3: the knockback lasts as long as the pace makes it", () => {
    const el = card();
    applyCardKnockback(el, 0, 30, 3, 2);
    vi.advanceTimersByTime(CARD_KNOCKBACK_MS + FRAME_MS);
    expect(el.style.translate).not.toBe("");
    vi.advanceTimersByTime(CARD_KNOCKBACK_MS);
    expect(el.style.translate).toBe("");
  });
});
