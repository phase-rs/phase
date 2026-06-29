import type {
  TournamentSummary,
  TournamentView,
} from "../adapter/types";
import type { ServerInfo } from "../adapter/ws-adapter";
import {
  HandshakeError,
  openPhaseSocket,
  type PhaseSocket,
} from "./openPhaseSocket";

export interface CreateTournamentRequest {
  name: string;
  displayName: string;
  totalRounds?: number;
}

export interface CreatedTournament {
  tournamentCode: string;
  playerKey: string;
}

export interface TournamentClient {
  readonly serverInfo: ServerInfo;
  createTournament(req: CreateTournamentRequest): Promise<CreatedTournament>;
  joinTournament(tournamentCode: string, displayName: string): Promise<TournamentView>;
  dropFromTournament(tournamentCode: string): Promise<void>;
  startRound(tournamentCode: string): Promise<void>;
  reportMatchResult(
    tournamentCode: string,
    matchId: string,
    winnerPlayerKey: string | null,
    playerAWins: number,
    playerBWins: number,
  ): Promise<void>;
  endTournament(tournamentCode: string): Promise<void>;
  listTournaments(): Promise<TournamentSummary[]>;
  subscribeTournaments(
    onList: (tournaments: TournamentSummary[]) => void,
    onUpdate: (tournament: TournamentView) => void,
    onCompleted: (tournamentCode: string) => void,
  ): () => void;
  close(): void;
}

export interface OpenTournamentClientOptions {
  signal?: AbortSignal;
  timeoutMs?: number;
}

export async function openTournamentClient(
  wsUrl: string,
  opts: OpenTournamentClientOptions = {},
): Promise<TournamentClient> {
  const socket = await openPhaseSocket(wsUrl, opts);
  if (socket.serverInfo.mode !== "LobbyOnly") {
    socket.close();
    throw new HandshakeError(
      "protocol_mismatch",
      `Expected LobbyOnly server, got ${socket.serverInfo.mode}`,
    );
  }
  return makeTournamentClient(socket);
}

function sendRpc<T>(
  socket: PhaseSocket,
  frame: object,
  match: (msg: { type: string; data?: unknown }) => T | null,
): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const { ws } = socket;
    const timeout = window.setTimeout(() => {
      cleanup();
      reject(new Error("Tournament RPC timed out"));
    }, 15_000);

    const listener = (event: MessageEvent) => {
      let msg: { type: string; data?: unknown };
      try {
        msg = JSON.parse(event.data as string) as { type: string; data?: unknown };
      } catch {
        return;
      }
      if (msg.type === "Error") {
        cleanup();
        const data = msg.data as { message: string };
        reject(new Error(data.message));
        return;
      }
      const result = match(msg);
      if (result !== null) {
        cleanup();
        resolve(result);
      }
    };

    const cleanup = () => {
      window.clearTimeout(timeout);
      ws.removeEventListener("message", listener);
    };

    ws.addEventListener("message", listener);
    ws.send(JSON.stringify(frame));
  });
}

function camelizeTournamentSummary(raw: Record<string, unknown>): TournamentSummary {
  return {
    tournamentCode: String(raw.tournament_code ?? raw.tournamentCode ?? ""),
    name: String(raw.name ?? ""),
    organizerName: String(raw.organizer_name ?? raw.organizerName ?? ""),
    createdAt: Number(raw.created_at ?? raw.createdAt ?? 0),
    status: (raw.status as TournamentSummary["status"]) ?? "registration",
    playerCount: Number(raw.player_count ?? raw.playerCount ?? 0),
    totalRounds: Number(raw.total_rounds ?? raw.totalRounds ?? 3),
    currentRound: Number(raw.current_round ?? raw.currentRound ?? 0),
  };
}

function camelizeStanding(raw: Record<string, unknown>): import("../adapter/types").TournamentStanding {
  return {
    playerKey: String(raw.player_key ?? raw.playerKey ?? ""),
    displayName: String(raw.display_name ?? raw.displayName ?? ""),
    dropped: Boolean(raw.dropped),
    matchPoints: Number(raw.match_points ?? raw.matchPoints ?? 0),
    matchWins: Number(raw.match_wins ?? raw.matchWins ?? 0),
    matchLosses: Number(raw.match_losses ?? raw.matchLosses ?? 0),
    matchDraws: Number(raw.match_draws ?? raw.matchDraws ?? 0),
    gameWins: Number(raw.game_wins ?? raw.gameWins ?? 0),
    gameLosses: Number(raw.game_losses ?? raw.gameLosses ?? 0),
    omwPercentage: Number(raw.omw_percentage ?? raw.omwPercentage ?? 0),
    gwPercentage: Number(raw.gw_percentage ?? raw.gwPercentage ?? 0),
    ogwPercentage: Number(raw.ogw_percentage ?? raw.ogwPercentage ?? 0),
  };
}

function camelizePairing(raw: Record<string, unknown>): import("../adapter/types").PairingView {
  return {
    matchId: String(raw.match_id ?? raw.matchId ?? ""),
    round: Number(raw.round ?? 0),
    table: Number(raw.table ?? 0),
    playerAKey: String(raw.player_a_key ?? raw.playerAKey ?? ""),
    playerAName: String(raw.player_a_name ?? raw.playerAName ?? ""),
    playerBKey: (raw.player_b_key ?? raw.playerBKey ?? null) as string | null,
    playerBName: (raw.player_b_name ?? raw.playerBName ?? null) as string | null,
    reported: Boolean(raw.reported),
    winnerPlayerKey: (raw.winner_player_key ?? raw.winnerPlayerKey ?? null) as string | null,
  };
}

function camelizeTournamentView(raw: Record<string, unknown>): TournamentView {
  const standings = Array.isArray(raw.standings)
    ? raw.standings.map((s) => camelizeStanding(s as Record<string, unknown>))
    : [];
  const pairings = Array.isArray(raw.pairings)
    ? raw.pairings.map((p) => camelizePairing(p as Record<string, unknown>))
    : [];
  return {
    tournamentCode: String(raw.tournament_code ?? raw.tournamentCode ?? ""),
    name: String(raw.name ?? ""),
    organizerName: String(raw.organizer_name ?? raw.organizerName ?? ""),
    createdAt: Number(raw.created_at ?? raw.createdAt ?? 0),
    status: (raw.status as TournamentView["status"]) ?? "registration",
    totalRounds: Number(raw.total_rounds ?? raw.totalRounds ?? 3),
    currentRound: Number(raw.current_round ?? raw.currentRound ?? 0),
    playerCount: Number(raw.player_count ?? raw.playerCount ?? 0),
    standings,
    pairings,
  };
}

function makeTournamentClient(socket: PhaseSocket): TournamentClient {
  return {
    serverInfo: socket.serverInfo,

    createTournament(req) {
      return sendRpc(socket, {
        type: "CreateTournament",
        data: {
          name: req.name,
          display_name: req.displayName,
          total_rounds: req.totalRounds ?? 3,
        },
      }, (msg) => {
        if (msg.type !== "TournamentCreated") return null;
        const data = msg.data as { tournament_code: string; player_key: string };
        return {
          tournamentCode: data.tournament_code,
          playerKey: data.player_key,
        };
      });
    },

    joinTournament(tournamentCode, displayName) {
      return sendRpc(socket, {
        type: "JoinTournament",
        data: { tournament_code: tournamentCode, display_name: displayName },
      }, (msg) => {
        if (msg.type !== "TournamentUpdate") return null;
        const data = msg.data as { tournament: Record<string, unknown> };
        return camelizeTournamentView(data.tournament);
      });
    },

    dropFromTournament(tournamentCode) {
      return sendRpc(socket, {
        type: "DropFromTournament",
        data: { tournament_code: tournamentCode },
      }, (msg) => {
        if (msg.type === "Error") return null;
        if (msg.type === "TournamentUpdate") return undefined as unknown as void;
        return null;
      }).then(() => undefined);
    },

    startRound(tournamentCode) {
      return sendRpc(socket, {
        type: "StartTournamentRound",
        data: { tournament_code: tournamentCode },
      }, (msg) => {
        if (msg.type === "TournamentUpdate" || msg.type === "TournamentCompleted") {
          return undefined as unknown as void;
        }
        return null;
      }).then(() => undefined);
    },

    reportMatchResult(tournamentCode, matchId, winnerPlayerKey, playerAWins, playerBWins) {
      return sendRpc(socket, {
        type: "ReportMatchResult",
        data: {
          tournament_code: tournamentCode,
          match_id: matchId,
          winner_player_key: winnerPlayerKey,
          player_a_wins: playerAWins,
          player_b_wins: playerBWins,
        },
      }, (msg) => {
        if (msg.type === "TournamentUpdate") return undefined as unknown as void;
        return null;
      }).then(() => undefined);
    },

    endTournament(tournamentCode) {
      return sendRpc(socket, {
        type: "EndTournament",
        data: { tournament_code: tournamentCode },
      }, (msg) => {
        if (msg.type === "TournamentCompleted" || msg.type === "TournamentUpdate") {
          return undefined as unknown as void;
        }
        return null;
      }).then(() => undefined);
    },

    listTournaments() {
      return sendRpc(socket, { type: "ListTournaments" }, (msg) => {
        if (msg.type !== "TournamentListUpdate") return null;
        const data = msg.data as { tournaments: Record<string, unknown>[] };
        return data.tournaments.map(camelizeTournamentSummary);
      });
    },

    subscribeTournaments(onList, onUpdate, onCompleted) {
      const { ws } = socket;
      const listener = (event: MessageEvent) => {
        let msg: { type: string; data?: unknown };
        try {
          msg = JSON.parse(event.data as string) as { type: string; data?: unknown };
        } catch {
          return;
        }
        switch (msg.type) {
          case "TournamentListUpdate": {
            const data = msg.data as { tournaments: Record<string, unknown>[] };
            onList(data.tournaments.map(camelizeTournamentSummary));
            break;
          }
          case "TournamentUpdate": {
            const data = msg.data as { tournament: Record<string, unknown> };
            onUpdate(camelizeTournamentView(data.tournament));
            break;
          }
          case "TournamentCompleted": {
            const data = msg.data as { tournament_code: string };
            onCompleted(data.tournament_code);
            break;
          }
        }
      };
      ws.addEventListener("message", listener);
      if (ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: "SubscribeLobby" }));
      }
      return () => {
        ws.removeEventListener("message", listener);
        if (ws.readyState === WebSocket.OPEN) {
          ws.send(JSON.stringify({ type: "UnsubscribeLobby" }));
        }
      };
    },

    close() {
      socket.close();
    },
  };
}
