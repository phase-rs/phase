export interface RawDiscordMessage {
  source: "discord";
  guild_id: string;
  channel_id: string;
  thread_id: string;
  thread_name: string;
  message_id: string;
  timestamp: string;
  edited_timestamp: string | null;
  author_id: string;
  author_name: string;
  author_is_bot: boolean;
  content: string;
  attachments: Array<{
    id: string;
    filename: string;
    url: string;
    content_type: string | null;
    size: number;
  }>;
  embeds: Array<{
    title: string | null;
    description: string | null;
    url: string | null;
    type: string | null;
  }>;
  referenced_message_id: string | null;
  fetched_at: string;
  content_hash: string;
}

export interface ReportItem {
  report_id: string;
  source: "discord" | "github";
  thread_id: string;
  thread_name: string;
  message_id: string;
  item_index: number;
  reported_at: string;
  author_name: string;
  cards: string[];
  mechanics: string[];
  summary: string;
  actual: string;
  expected: string;
  evidence: {
    source_url: string;
    attachments: RawDiscordMessage["attachments"];
    raw_content_hash: string;
  };
  extraction_confidence: number;
  status: "unlinked" | "linked" | "duplicate" | "stale" | "ignored";
}

export type TriageClassification =
  | "primary_report"
  | "additional_report"
  | "follow_up"
  | "developer_reply"
  | "correction"
  | "chatter"
  | "evidence_only"
  | "stale_likely_fixed";

export interface TriageItem {
  report_id: string;
  classification: TriageClassification;
  reason: string;
  thread_id: string;
  thread_name: string;
  message_id: string;
  cards: string[];
  summary: string;
  extraction_confidence: number;
  source_url: string;
  parser_status: "fully_parsed" | "has_gaps" | "unknown_card" | "no_card";
  proposed_action: "create_issue" | "append_to_existing" | "skip" | "needs_human_review";
  dedup_group: string | null;
}

export interface SyncState {
  last_fetch_at: string;
  last_thread_cursors: Record<string, string>;
  imported_from_legacy: boolean;
}
