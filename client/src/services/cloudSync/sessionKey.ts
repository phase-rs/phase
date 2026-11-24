import { SUPABASE_CONFIG } from "./supabaseConfig";

/** Derive Supabase's default localStorage key from a configured project URL. */
export function supabaseSessionKeyFromUrl(url: string): string | null {
  try {
    const projectRef = new URL(url).hostname.split(".")[0];
    return projectRef ? `sb-${projectRef}-auth-token` : null;
  } catch {
    return null;
  }
}

/** The localStorage key supabase-js uses for this deployment's auth session. */
export function getSupabaseSessionKey(): string | null {
  return supabaseSessionKeyFromUrl(SUPABASE_CONFIG.url);
}
