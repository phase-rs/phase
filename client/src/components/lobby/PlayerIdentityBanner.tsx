import { useEffect, useRef, useState } from "react";

import { useMultiplayerStore } from "../../stores/multiplayerStore";

/**
 * Inline player-name editor rendered at the top of the multiplayer flow.
 * Clicking "Change" (or the name itself) reveals an input without
 * navigating into Preferences → Multiplayer, which used to be the only
 * place to edit the display name.
 *
 * Edits commit to the multiplayer store on Enter / blur / "Save". Escape or
 * clicking "Cancel" reverts to the previous value. The store is persisted,
 * so any edit here is the authoritative identity for future host/join
 * actions.
 */
export function PlayerIdentityBanner() {
  const displayName = useMultiplayerStore((s) => s.displayName);
  const setDisplayName = useMultiplayerStore((s) => s.setDisplayName);

  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(displayName);
  const inputRef = useRef<HTMLInputElement>(null);

  // Re-sync draft whenever the store value changes from outside this
  // component (e.g. the user edits their name in HostSetup, then navigates
  // back here).
  useEffect(() => {
    if (!editing) setDraft(displayName);
  }, [displayName, editing]);

  useEffect(() => {
    if (editing) {
      inputRef.current?.focus();
      inputRef.current?.select();
    }
  }, [editing]);

  const commit = () => {
    const trimmed = draft.trim();
    if (trimmed && trimmed !== displayName) {
      setDisplayName(trimmed);
    } else if (!trimmed) {
      setDraft(displayName);
    }
    setEditing(false);
  };

  const cancel = () => {
    setDraft(displayName);
    setEditing(false);
  };

  return (
    <div className="mx-auto mb-4 flex w-full max-w-xl items-center justify-between gap-3 rounded-[16px] border border-white/8 bg-black/16 px-4 py-2.5">
      <div className="min-w-0 flex-1">
        <div className="text-[0.6rem] uppercase tracking-[0.22em] text-slate-500">
          Player Name
        </div>
        {editing ? (
          <input
            ref={inputRef}
            type="text"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onBlur={commit}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                commit();
              } else if (e.key === "Escape") {
                e.preventDefault();
                cancel();
              }
            }}
            placeholder="Enter your name"
            maxLength={20}
            className="mt-0.5 w-full rounded-[12px] bg-black/24 px-2.5 py-1 text-sm text-white placeholder-gray-500 outline-none ring-1 ring-white/10 focus:ring-white/20"
          />
        ) : (
          <button
            type="button"
            onClick={() => setEditing(true)}
            className="mt-0.5 truncate text-left text-sm font-medium text-white transition-colors hover:text-sky-200"
            title="Click to edit your name"
          >
            {displayName || (
              <span className="italic text-slate-500">Set a name…</span>
            )}
          </button>
        )}
      </div>
      {editing ? (
        <div className="flex shrink-0 gap-2">
          <button
            type="button"
            // onMouseDown so it fires before the input's onBlur cancels.
            onMouseDown={(e) => {
              e.preventDefault();
              cancel();
            }}
            className="text-xs text-slate-500 transition-colors hover:text-slate-300"
          >
            Cancel
          </button>
          <button
            type="button"
            onMouseDown={(e) => {
              e.preventDefault();
              commit();
            }}
            className="text-xs font-medium text-sky-300 transition-colors hover:text-sky-200"
          >
            Save
          </button>
        </div>
      ) : (
        <button
          type="button"
          onClick={() => setEditing(true)}
          className="shrink-0 text-xs text-slate-400 transition-colors hover:text-white"
        >
          Change
        </button>
      )}
    </div>
  );
}
