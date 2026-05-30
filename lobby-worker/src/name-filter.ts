type JsonRecord = Record<string, unknown>;

const DISPLAY_NAME_MAX_LENGTH = 20;
const ROOM_NAME_MAX_LENGTH = 40;
const CONTROL_CHARACTER_PATTERN = /[\u0000-\u001f\u007f]/u;
const URL_PATTERN = /\b(?:https?:\/\/|www\.)/iu;

const BLOCKED_COMPACT_SUBSTRINGS = [
  "cunt",
  "fuck",
  "faggot",
  "nigger",
  "nigga",
  "killurself",
  "killyourself",
] as const;

const BLOCKED_WORDS = [
  "bitch",
  "chink",
  "coon",
  "dyke",
  "fag",
  "gook",
  "hitler",
  "kike",
  "kys",
  "nazi",
  "rape",
  "retard",
  "shit",
  "spic",
  "tranny",
  "wetback",
] as const;

const LEET_REPLACEMENTS: Record<string, string> = {
  "0": "o",
  "1": "i",
  "3": "e",
  "4": "a",
  "5": "s",
  "7": "t",
  "@": "a",
  "$": "s",
  "!": "i",
};

export function moderationErrorForLobbyFrame(rawFrame: string): string | null {
  const frame = parseFrame(rawFrame);
  if (!frame) return null;

  switch (frame.type) {
    case "CreateGameWithSettings": {
      const data = asRecord(frame.data);
      if (!data) return null;
      return (
        validateVisibleLabel(data.display_name, "Player name", DISPLAY_NAME_MAX_LENGTH)
        ?? validateOptionalVisibleLabel(data.room_name, "Room name", ROOM_NAME_MAX_LENGTH)
      );
    }
    case "JoinGameWithPassword": {
      const data = asRecord(frame.data);
      if (!data) return null;
      return validateVisibleLabel(data.display_name, "Player name", DISPLAY_NAME_MAX_LENGTH);
    }
    case "LookupJoinTarget": {
      const data = asRecord(frame.data);
      if (!data) return null;
      return validateOptionalVisibleLabel(data.display_name, "Player name", DISPLAY_NAME_MAX_LENGTH);
    }
    default:
      return null;
  }
}

function parseFrame(rawFrame: string): JsonRecord | null {
  try {
    return asRecord(JSON.parse(rawFrame));
  } catch {
    return null;
  }
}

function asRecord(value: unknown): JsonRecord | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as JsonRecord
    : null;
}

function validateOptionalVisibleLabel(
  value: unknown,
  label: string,
  maxLength: number,
): string | null {
  if (value == null) return null;
  return validateVisibleLabel(value, label, maxLength);
}

function validateVisibleLabel(
  value: unknown,
  label: string,
  maxLength: number,
): string | null {
  if (typeof value !== "string") return null;

  const trimmed = value.trim();
  if (!trimmed) return null;
  if (Array.from(trimmed).length > maxLength) {
    return `${label} must be ${maxLength} characters or fewer.`;
  }
  if (CONTROL_CHARACTER_PATTERN.test(trimmed)) {
    return `${label} contains unsupported characters.`;
  }
  if (URL_PATTERN.test(trimmed)) {
    return `${label} cannot include links.`;
  }
  if (containsBlockedTerm(trimmed)) {
    return `${label} is not allowed on the public lobby.`;
  }

  return null;
}

function containsBlockedTerm(value: string): boolean {
  const normalized = normalizeForModeration(value);
  const compact = normalized.replace(/[^a-z0-9]/gu, "");
  if (BLOCKED_COMPACT_SUBSTRINGS.some((term) => compact.includes(term))) {
    return true;
  }

  const words = normalized.split(/[^a-z0-9]+/u).filter(Boolean);
  return BLOCKED_WORDS.some((term) => words.includes(term));
}

function normalizeForModeration(value: string): string {
  return value
    .normalize("NFKD")
    .replace(/\p{M}/gu, "")
    .toLowerCase()
    .replace(/[013457@$!]/gu, (char) => LEET_REPLACEMENTS[char] ?? char);
}
