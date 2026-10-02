/**
 * The latest client-side AI decision stage, included in display snapshots so
 * a report can distinguish a waiting engine from a controller that never
 * began a request. This is diagnostic state only; it never drives gameplay.
 */
export type AiDecisionStage = "idle" | "awaiting-proposal" | "submitting-proposal" | "failed";

export interface AiDecisionDiagnostic {
  stage: AiDecisionStage;
  playerId: number | null;
  difficulty: string | null;
  waitingFor: string | null;
  error?: string;
}

let latestAiDecisionDiagnostic: AiDecisionDiagnostic = {
  stage: "idle",
  playerId: null,
  difficulty: null,
  waitingFor: null,
};

export function recordAiDecisionDiagnostic(diagnostic: AiDecisionDiagnostic): void {
  latestAiDecisionDiagnostic = diagnostic;
}

export function currentAiDecisionDiagnostic(): AiDecisionDiagnostic {
  return latestAiDecisionDiagnostic;
}

export function clearAiDecisionDiagnostic(): void {
  latestAiDecisionDiagnostic = {
    stage: "idle",
    playerId: null,
    difficulty: null,
    waitingFor: null,
  };
}
