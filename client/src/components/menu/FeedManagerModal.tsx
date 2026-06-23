import { useState } from "react";
import { createPortal } from "react-dom";
import { motion, AnimatePresence } from "framer-motion";
import { useTranslation } from "react-i18next";

import { menuButtonClass } from "./buttonStyles";
import { FEED_REGISTRY } from "../../data/feedRegistry";
import {
  listSubscriptions,
  subscribe,
  unsubscribe,
  refreshFeed,
  refreshAllFeeds,
} from "../../services/feedService";
import type { FeedSubscription } from "../../types/feed";

interface FeedManagerModalProps {
  open: boolean;
  onClose: () => void;
}

export function FeedManagerModal({ open, onClose }: FeedManagerModalProps) {
  const { t } = useTranslation("menu");
  const [subs, setSubs] = useState<FeedSubscription[]>(() => listSubscriptions());
  const [customUrl, setCustomUrl] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState<string | null>(null);

  const subscribedIds = new Set(subs.map((s) => s.sourceId));

  const handleSubscribe = async (sourceId: string) => {
    setLoading(sourceId);
    setError(null);
    try {
      await subscribe(sourceId);
      setSubs(listSubscriptions());
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(null);
    }
  };

  const handleUnsubscribe = (feedId: string) => {
    unsubscribe(feedId);
    setSubs(listSubscriptions());
  };

  const handleRefresh = async (feedId: string) => {
    setLoading(feedId);
    setError(null);
    try {
      await refreshFeed(feedId);
      setSubs(listSubscriptions());
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(null);
    }
  };

  const handleRefreshAll = async () => {
    setLoading("all");
    setError(null);
    try {
      await refreshAllFeeds();
      setSubs(listSubscriptions());
    } finally {
      setLoading(null);
    }
  };

  const handleCustomSubscribe = async () => {
    const url = customUrl.trim();
    if (!url) return;
    setLoading("custom");
    setError(null);
    try {
      await subscribe(url);
      setSubs(listSubscriptions());
      setCustomUrl("");
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(null);
    }
  };

  return createPortal(
    <AnimatePresence>
      {open && (
        <motion.div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          onClick={onClose}
        >
          <motion.div
            className="mx-4 flex max-h-[90vh] w-full max-w-lg flex-col rounded-2xl border border-white/10 bg-[#0c1120] shadow-2xl"
            initial={{ scale: 0.95, opacity: 0 }}
            animate={{ scale: 1, opacity: 1 }}
            exit={{ scale: 0.95, opacity: 0 }}
            onClick={(e) => e.stopPropagation()}
          >
            <div className="shrink-0 flex flex-col gap-3 px-4 pb-4 pt-6 sm:flex-row sm:items-center sm:justify-between sm:px-6">
              <h2 className="text-lg font-semibold text-white">{t("feedManager.title")}</h2>
              <button
                onClick={handleRefreshAll}
                disabled={loading === "all" || subs.length === 0}
                className="flex min-h-11 items-center justify-center self-start rounded px-3 py-1.5 text-xs text-slate-300 ring-1 ring-white/10 transition-colors hover:bg-white/5 hover:text-white disabled:opacity-40 sm:min-h-0 sm:self-auto"
              >
                {loading === "all" ? t("feedManager.refreshing") : t("feedManager.refreshAll")}
              </button>
            </div>

            <div className="min-h-0 flex-1 overflow-y-auto px-4 pb-4 sm:px-6">
            {error && (
              <div className="mb-4 rounded-lg border border-red-500/30 bg-red-500/10 px-3 py-2 text-xs text-red-200">
                {error}
              </div>
            )}

            <div className="flex flex-col gap-3">
              {FEED_REGISTRY.map((source) => {
                const isSubscribed = subscribedIds.has(source.id);
                const sub = subs.find((s) => s.sourceId === source.id);
                const isLoading = loading === source.id;

                return (
                  <div
                    key={source.id}
                    className="flex flex-col gap-3 rounded-xl border border-white/10 bg-black/20 px-4 py-3 sm:flex-row sm:items-center sm:justify-between"
                  >
                    <div className="min-w-0 flex-1">
                      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                        {source.icon && (
                          <span className="inline-flex h-5 w-5 shrink-0 items-center justify-center rounded bg-white/10 text-[10px] font-bold text-white">
                            {source.icon}
                          </span>
                        )}
                        <span className="text-sm font-medium text-white">{source.name}</span>
                        <span className="rounded bg-white/5 px-1.5 py-0.5 text-[10px] text-slate-500">
                          {source.type}
                        </span>
                      </div>
                      {source.description && (
                        <p className="mt-0.5 text-xs text-slate-500">{source.description}</p>
                      )}
                      {sub && (
                        <p className="mt-0.5 text-[10px] text-slate-600">
                          {t("feedManager.lastRefreshed", { date: new Date(sub.lastRefreshedAt).toLocaleDateString() })}
                          {sub.error && <span className="ml-2 text-red-400">{sub.error}</span>}
                        </p>
                      )}
                    </div>
                    <div className="flex shrink-0 gap-2 sm:ml-3">
                      {isSubscribed && (
                        <button
                          onClick={() => handleRefresh(source.id)}
                          disabled={isLoading}
                          className="min-h-11 flex-1 rounded px-3 py-1.5 text-xs text-slate-400 ring-1 ring-white/10 transition-colors hover:bg-white/5 hover:text-white disabled:opacity-40 sm:min-h-0 sm:flex-none sm:px-2 sm:py-1"
                        >
                          {isLoading ? "…" : t("feedManager.refresh")}
                        </button>
                      )}
                      <button
                        onClick={() => isSubscribed ? handleUnsubscribe(source.id) : handleSubscribe(source.id)}
                        disabled={isLoading}
                        className={`min-h-11 flex-1 rounded px-3 py-1.5 text-xs font-medium transition-colors disabled:opacity-40 sm:min-h-0 sm:flex-none sm:py-1 ${
                          isSubscribed
                            ? "text-red-300 ring-1 ring-red-500/30 hover:bg-red-500/10"
                            : "text-emerald-300 ring-1 ring-emerald-500/30 hover:bg-emerald-500/10"
                        }`}
                      >
                        {isSubscribed ? t("feedManager.unsubscribe") : t("feedManager.subscribe")}
                      </button>
                    </div>
                  </div>
                );
              })}

              {/* Custom URL feeds */}
              {subs
                .filter((sub) => !FEED_REGISTRY.some((s) => s.id === sub.sourceId))
                .map((sub) => (
                  <div
                    key={sub.sourceId}
                    className="flex flex-col gap-3 rounded-xl border border-white/10 bg-black/20 px-4 py-3 sm:flex-row sm:items-center sm:justify-between"
                  >
                    <div className="min-w-0 flex-1">
                      <div className="text-sm font-medium text-white">{sub.sourceId}</div>
                      <p className="mt-0.5 truncate text-[10px] text-slate-600">{sub.url}</p>
                    </div>
                    <div className="flex shrink-0 gap-2 sm:ml-3">
                      <button
                        onClick={() => handleRefresh(sub.sourceId)}
                        disabled={loading === sub.sourceId}
                        className="min-h-11 flex-1 rounded px-3 py-1.5 text-xs text-slate-400 ring-1 ring-white/10 transition-colors hover:bg-white/5 hover:text-white disabled:opacity-40 sm:min-h-0 sm:flex-none sm:px-2 sm:py-1"
                      >
                        {loading === sub.sourceId ? "…" : t("feedManager.refresh")}
                      </button>
                      <button
                        onClick={() => handleUnsubscribe(sub.sourceId)}
                        className="min-h-11 flex-1 rounded px-3 py-1.5 text-xs font-medium text-red-300 ring-1 ring-red-500/30 transition-colors hover:bg-red-500/10 sm:min-h-0 sm:flex-none sm:py-1"
                      >
                        {t("feedManager.unsubscribe")}
                      </button>
                    </div>
                  </div>
                ))}
            </div>

            <div className="mt-4 border-t border-white/10 pt-4">
              <h3 className="mb-2 text-xs font-semibold uppercase tracking-wider text-slate-400">{t("feedManager.addCustomFeed")}</h3>
              <div className="flex flex-col gap-2 sm:flex-row">
                <input
                  type="url"
                  value={customUrl}
                  onChange={(e) => setCustomUrl(e.target.value)}
                  placeholder="https://example.com/feed.json"
                  className="min-w-0 flex-1 rounded-lg border border-white/10 bg-black/30 px-3 py-2 text-sm text-white placeholder-slate-600 outline-none focus:border-white/20"
                  onKeyDown={(e) => e.key === "Enter" && handleCustomSubscribe()}
                />
                <button
                  onClick={handleCustomSubscribe}
                  disabled={!customUrl.trim() || loading === "custom"}
                  className={`${menuButtonClass({ tone: "indigo", size: "sm", disabled: !customUrl.trim() || loading === "custom" })} w-full sm:w-auto`}
                >
                  {loading === "custom" ? "…" : t("feedManager.add")}
                </button>
              </div>
            </div>

            </div>

            <div className="shrink-0 flex justify-end border-t border-white/10 px-4 py-4 sm:px-6">
              <button
                onClick={onClose}
                className={menuButtonClass({ tone: "neutral", size: "sm" })}
              >
                {t("feedManager.done")}
              </button>
            </div>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>,
    document.body,
  );
}
