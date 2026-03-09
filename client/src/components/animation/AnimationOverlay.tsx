import { AnimatePresence } from "framer-motion";
import { useCallback, useEffect, useRef, useState } from "react";

import type { StepEffect } from "../../animation/types.ts";
import { useAnimationStore } from "../../stores/animationStore.ts";
import { FloatingNumber } from "./FloatingNumber.tsx";
import { ParticleCanvas } from "./ParticleCanvas.tsx";
import type { ParticleCanvasHandle } from "./ParticleCanvas.tsx";

interface ActiveFloat {
  id: number;
  value: number;
  position: { x: number; y: number };
  color: string;
}

let floatIdCounter = 0;

export function AnimationOverlay() {
  const steps = useAnimationStore((s) => s.steps);
  const isPlaying = useAnimationStore((s) => s.isPlaying);
  const playNextStep = useAnimationStore((s) => s.playNextStep);
  const getPosition = useAnimationStore((s) => s.getPosition);
  const particleRef = useRef<ParticleCanvasHandle>(null);
  const [activeFloats, setActiveFloats] = useState<ActiveFloat[]>([]);
  const processingRef = useRef(false);

  const processEffect = useCallback(
    (effect: StepEffect) => {
      const data = effect.data as Record<string, unknown>;

      switch (effect.type) {
        case "DamageDealt": {
          const target = data.target as
            | { Object: number }
            | { Player: number }
            | undefined;
          const amount = (data.amount as number) ?? 0;
          let pos = { x: window.innerWidth / 2, y: window.innerHeight / 2 };

          if (target && "Object" in target) {
            const rect = getPosition(target.Object);
            if (rect) pos = { x: rect.x + rect.width / 2, y: rect.y };
          }

          const id = ++floatIdCounter;
          setActiveFloats((prev) => [
            ...prev,
            { id, value: -amount, position: pos, color: "#ef4444" },
          ]);
          break;
        }

        case "LifeChanged": {
          const amount = (data.amount as number) ?? 0;
          // Position near life total area -- player 0 bottom-right, player 1 top-right
          const playerId = (data.player_id as number) ?? 0;
          const y = playerId === 0 ? window.innerHeight - 120 : 80;
          const x = window.innerWidth - 140;

          const id = ++floatIdCounter;
          setActiveFloats((prev) => [
            ...prev,
            {
              id,
              value: amount,
              position: { x, y },
              color: amount > 0 ? "#22c55e" : "#ef4444",
            },
          ]);
          break;
        }

        case "AttackersDeclared": {
          const attackerIds = (data.attacker_ids as number[]) ?? [];
          for (const attackerId of attackerIds) {
            const rect = getPosition(attackerId);
            if (rect) {
              particleRef.current?.emitBurst(
                rect.x + rect.width / 2,
                rect.y + rect.height / 2,
                "#ffffff",
                8,
              );
            }
          }
          break;
        }

        case "CreatureDestroyed": {
          const objectId = data.object_id as number | undefined;
          if (objectId != null) {
            const rect = getPosition(objectId);
            if (rect) {
              particleRef.current?.emitBurst(
                rect.x + rect.width / 2,
                rect.y + rect.height / 2,
                "#ef4444",
                16,
              );
            }
          }
          break;
        }

        case "SpellCast": {
          const cardId = data.card_id as number | undefined;
          if (cardId != null) {
            const rect = getPosition(cardId);
            if (rect) {
              particleRef.current?.emitBurst(
                rect.x + rect.width / 2,
                rect.y + rect.height / 2,
                "#06b6d4",
                12,
              );
            }
          }
          break;
        }

        default:
          // ZoneChanged and other effects rely on layout animations
          break;
      }
    },
    [getPosition],
  );

  useEffect(() => {
    if (!isPlaying || steps.length === 0 || processingRef.current) return;

    processingRef.current = true;
    const step = playNextStep();
    if (!step) {
      processingRef.current = false;
      return;
    }

    for (const effect of step.effects) {
      processEffect(effect);
    }

    const timer = setTimeout(() => {
      processingRef.current = false;
    }, step.duration);

    return () => clearTimeout(timer);
  }, [isPlaying, steps, playNextStep, processEffect]);

  const handleFloatComplete = useCallback((id: number) => {
    setActiveFloats((prev) => prev.filter((f) => f.id !== id));
  }, []);

  return (
    <>
      <ParticleCanvas ref={particleRef} />
      <AnimatePresence>
        {activeFloats.map((f) => (
          <FloatingNumber
            key={f.id}
            value={f.value}
            position={f.position}
            color={f.color}
            onComplete={() => handleFloatComplete(f.id)}
          />
        ))}
      </AnimatePresence>
    </>
  );
}
