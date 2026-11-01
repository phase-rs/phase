import { useMemo } from "react";

import { loadDeckMetaMap } from "../constants/storage";
import { buildFolderIndex, type FolderIndex } from "../services/randomPoolSelection";
import { useDeckFolders } from "./useDeckFolders";

/**
 * Reactive folder index for pool resolution. `folders` identity refreshes on
 * every library change (own writes, other tabs, cloud-sync replaces), so
 * memos depending on this index recompute with current metadata instead of
 * serving stale membership. Pass it through every resolution and summary
 * boundary (`applyPoolSelection`, `resolveRandomDrawPool`,
 * `resolveEffectiveAiRandomPool`, `summarizePoolSelection`) and include it in
 * the memo dependencies. (`RandomPoolTree` inlines the equivalent because it
 * also needs the folder list itself for section labels.)
 */
export function usePoolFolderIndex(): FolderIndex {
  const { folders } = useDeckFolders();
  return useMemo(() => buildFolderIndex(folders, loadDeckMetaMap()), [folders]);
}
