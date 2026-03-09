export interface ShakeConfig {
  amplitude: number;
  duration: number;
  oscillations: number;
}

const SHAKE_CONFIGS: Record<"light" | "medium" | "heavy", ShakeConfig> = {
  light: { amplitude: 2, duration: 150, oscillations: 4 },
  medium: { amplitude: 4, duration: 250, oscillations: 5 },
  heavy: { amplitude: 8, duration: 350, oscillations: 6 },
};

export function getShakeConfig(
  intensity: "light" | "medium" | "heavy",
): ShakeConfig {
  return SHAKE_CONFIGS[intensity];
}

export function applyScreenShake(
  element: HTMLElement,
  intensity: "light" | "medium" | "heavy",
  speedMultiplier: number,
): void {
  const config = SHAKE_CONFIGS[intensity];
  const duration = config.duration * speedMultiplier;
  const start = performance.now();

  const frame = (now: number) => {
    const elapsed = now - start;
    const progress = elapsed / duration;

    if (progress >= 1) {
      element.style.transform = "";
      return;
    }

    const decay = 1 - progress;
    const angle = progress * config.oscillations * Math.PI * 2;
    const offsetX = Math.sin(angle) * config.amplitude * decay;
    const offsetY = Math.sin(angle) * (config.amplitude * 0.5) * decay;
    element.style.transform = `translate(${offsetX}px, ${offsetY}px)`;

    requestAnimationFrame(frame);
  };

  requestAnimationFrame(frame);
}
