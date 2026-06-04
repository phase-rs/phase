import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";

import type { MatchRecord } from "../../services/matchHistoryPersistence";
import { MatchRecordCard } from "./MatchRecordCard";

const PAGE_SIZE = 25;

interface MatchListProps {
  records: MatchRecord[];
  allRecordsCount: number;
  onDelete: (id: string) => void;
  onResetFilters?: () => void;
}

export function MatchList({ records, allRecordsCount, onDelete, onResetFilters }: MatchListProps) {
  const { t } = useTranslation("history");
  const navigate = useNavigate();
  const [page, setPage] = useState(0);

  const totalPages = Math.max(1, Math.ceil(records.length / PAGE_SIZE));
  const pageRecords = records.slice(page * PAGE_SIZE, (page + 1) * PAGE_SIZE);

  // If filters removed all records but there are records in total, show "no match" state.
  if (records.length === 0 && allRecordsCount > 0) {
    return (
      <div className="flex flex-col items-center gap-4 rounded-xl border border-slate-700/30 bg-slate-900/40 py-12 text-center">
        <svg viewBox="0 0 48 48" className="h-12 w-12 text-slate-700 fill-current">
          <path d="M24 4a20 20 0 1 0 0 40A20 20 0 0 0 24 4Zm0 36a16 16 0 1 1 0-32 16 16 0 0 1 0 32Zm-2-10h4v4h-4zm0-18h4v14h-4z" />
        </svg>
        <div className="flex flex-col gap-1">
          <span className="font-semibold text-slate-300">{t("emptyFiltered.title")}</span>
          <span className="text-sm text-slate-500">{t("emptyFiltered.description")}</span>
        </div>
        {onResetFilters && (
          <button
            type="button"
            onClick={onResetFilters}
            className="rounded-lg border border-slate-600/50 bg-slate-800/50 px-4 py-2 text-sm text-slate-300 hover:bg-slate-700/50"
          >
            {t("emptyFiltered.reset")}
          </button>
        )}
      </div>
    );
  }

  // If no records exist at all, show the empty state with CTA.
  if (allRecordsCount === 0) {
    return (
      <div className="flex flex-col items-center gap-4 rounded-xl border border-slate-700/30 bg-slate-900/40 py-16 text-center">
        <svg viewBox="0 0 48 48" className="h-14 w-14 text-slate-700 fill-current">
          <path d="M38 8H10a2 2 0 0 0-2 2v28a2 2 0 0 0 2 2h28a2 2 0 0 0 2-2V10a2 2 0 0 0-2-2Zm-2 28H12V12h24v24ZM23 17h2v9h-2zm0 11h2v2h-2z" />
        </svg>
        <div className="flex flex-col gap-1">
          <span className="text-lg font-semibold text-slate-200">{t("empty.title")}</span>
          <span className="text-sm text-slate-500">{t("empty.description")}</span>
        </div>
        <button
          type="button"
          onClick={() => navigate("/setup")}
          className="rounded-lg bg-slate-700 px-5 py-2 text-sm font-medium text-slate-200 transition-colors hover:bg-slate-600"
        >
          {t("empty.cta")}
        </button>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      {/* Results count */}
      <div className="flex items-center justify-between text-xs text-slate-500">
        <span>
          {t("header.gamesPlayed", { count: records.length })}
          {records.length !== allRecordsCount && ` (filtered from ${allRecordsCount})`}
        </span>
        {totalPages > 1 && (
          <span>
            Page {page + 1} of {totalPages}
          </span>
        )}
      </div>

      {/* Record cards */}
      <div className="flex flex-col gap-2">
        {pageRecords.map((record) => (
          <MatchRecordCard key={record.id} record={record} onDelete={onDelete} />
        ))}
      </div>

      {/* Pagination */}
      {totalPages > 1 && (
        <div className="flex items-center justify-center gap-2">
          <button
            type="button"
            disabled={page === 0}
            onClick={() => setPage((p) => Math.max(0, p - 1))}
            className="rounded-lg border border-slate-700/50 bg-slate-800/50 px-3 py-1.5 text-sm text-slate-300 disabled:opacity-40 hover:bg-slate-700/50"
          >
            Previous
          </button>

          {/* Page number pills */}
          <div className="flex items-center gap-1">
            {Array.from({ length: Math.min(7, totalPages) }, (_, i) => {
              // Show first page, last page, current ±2, with ellipsis
              let pageNum: number | null;
              if (totalPages <= 7) {
                pageNum = i;
              } else if (i === 0) {
                pageNum = 0;
              } else if (i === 6) {
                pageNum = totalPages - 1;
              } else if (i === 1 && page > 3) {
                pageNum = null; // ellipsis
              } else if (i === 5 && page < totalPages - 4) {
                pageNum = null; // ellipsis
              } else {
                const start = Math.min(Math.max(page - 1, 1), totalPages - 5);
                pageNum = start + (i - 1);
              }

              if (pageNum === null) {
                return (
                  <span key={i} className="px-1 text-slate-600">
                    …
                  </span>
                );
              }

              return (
                <button
                  key={i}
                  type="button"
                  onClick={() => setPage(pageNum!)}
                  className={`h-8 w-8 rounded-lg text-sm transition-colors ${
                    pageNum === page
                      ? "bg-slate-600 font-semibold text-white"
                      : "border border-slate-700/50 bg-slate-800/50 text-slate-400 hover:bg-slate-700/50"
                  }`}
                >
                  {pageNum + 1}
                </button>
              );
            })}
          </div>

          <button
            type="button"
            disabled={page >= totalPages - 1}
            onClick={() => setPage((p) => Math.min(totalPages - 1, p + 1))}
            className="rounded-lg border border-slate-700/50 bg-slate-800/50 px-3 py-1.5 text-sm text-slate-300 disabled:opacity-40 hover:bg-slate-700/50"
          >
            Next
          </button>
        </div>
      )}
    </div>
  );
}
