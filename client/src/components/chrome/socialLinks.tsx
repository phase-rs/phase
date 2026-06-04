import type { ComponentType, MouseEvent } from "react";

import { openExternal } from "../../services/openExternal";

const DISCORD_URL = "https://discord.gg/dUZwhYHUyk";
const GITHUB_URL = "https://github.com/phase-rs/phase";
const KOFI_URL = "https://ko-fi.com/phasers";
const SPONSOR_URL = "https://github.com/sponsors/matthewevans";

/** Intercept the click so external links open via the platform shell (Tauri /
 *  web) rather than navigating the SPA. */
export function social(url: string) {
  return (e: MouseEvent) => {
    e.preventDefault();
    openExternal(url);
  };
}

function GitHubGlyph() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="h-4 w-4 fill-current">
      <path d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0 1 12 6.844a9.59 9.59 0 0 1 2.504.337c1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.02 10.02 0 0 0 22 12.017C22 6.484 17.522 2 12 2Z" />
    </svg>
  );
}

function DiscordGlyph() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="h-4 w-4 fill-current">
      <path d="M19.3 5.3A16 16 0 0 0 15.3 4l-.2.4a12 12 0 0 1 3.4 1.7 11 11 0 0 0-9 0A12 12 0 0 1 12.9 4.4L12.7 4a16 16 0 0 0-4 1.3C2.7 10.7 3.2 16 3.2 16a16 16 0 0 0 4.9 2.5l.6-1.1a9 9 0 0 1-1.5-.7l.4-.3a11 11 0 0 0 9.4 0l.4.3a9 9 0 0 1-1.5.7l.6 1.1A16 16 0 0 0 20.8 16s.5-5.3-1.5-10.7ZM9.5 14c-.6 0-1.1-.6-1.1-1.3 0-.7.5-1.3 1.1-1.3s1.1.6 1.1 1.3c0 .7-.5 1.3-1.1 1.3Zm5 0c-.6 0-1.1-.6-1.1-1.3 0-.7.5-1.3 1.1-1.3s1.1.6 1.1 1.3c0 .7-.5 1.3-1.1 1.3Z" />
    </svg>
  );
}

function KoFiGlyph() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="h-4 w-4 fill-current">
      <path d="M22.5 8.5c-.3-2-1.9-3.5-3.9-3.8-.6-.1-1.2-.1-1.9-.1H4.8c-1 0-1.8.7-1.9 1.7-.2 1.7-.2 5.5.7 8.2.6 1.7 2 3 3.8 3.4.9.2 1.9.3 2.8.3h4c.9 0 1.8-.1 2.7-.3 1.4-.3 2.5-1.2 3.1-2.5h.2c2.4 0 4.3-2 4.3-4.3 0-1.4-.7-2.7-2-3.4v.8ZM9.4 13.3c-1.2-1.1-2.7-2.3-2.7-4 0-1 .8-1.8 1.8-1.8.6 0 1.2.3 1.6.8.4-.5.9-.8 1.6-.8 1 0 1.8.8 1.8 1.8 0 1.7-1.5 2.9-2.7 4l-.7.6-.7-.6Zm10.7-1.6c-.3.4-.8.6-1.3.7V9c.4 0 .7.1 1 .3.4.3.7.8.7 1.3s-.1.9-.4 1.1Z" />
    </svg>
  );
}

function SponsorGlyph() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="h-4 w-4 fill-current">
      <path d="M12 21s-7.5-4.6-10-9.3C.3 8.4 1.9 4.5 5.4 4c2-.3 3.9.6 5.1 2.2l1.5 1.9 1.5-1.9C14.7 4.6 16.6 3.7 18.6 4c3.5.5 5.1 4.4 3.4 7.7C19.5 16.4 12 21 12 21z" />
    </svg>
  );
}

export interface SocialLink {
  key: string;
  url: string;
  label: string;
  Glyph: ComponentType;
  /** Brand-tinted hover background + text utilities. */
  hover: string;
}

/** Single source for the project's external links, shared by the desktop rail
 *  (icon + label rows) and the mobile top icon strip. */
export const SOCIAL_LINKS: SocialLink[] = [
  { key: "github", url: GITHUB_URL, label: "GitHub", Glyph: GitHubGlyph, hover: "hover:bg-white/[0.06] hover:text-white" },
  { key: "discord", url: DISCORD_URL, label: "Discord", Glyph: DiscordGlyph, hover: "hover:bg-[rgba(88,101,242,0.14)] hover:text-[#7c88f5]" },
  { key: "kofi", url: KOFI_URL, label: "Ko-fi", Glyph: KoFiGlyph, hover: "hover:bg-[rgba(255,94,91,0.14)] hover:text-[#ff5e5b]" },
  { key: "sponsor", url: SPONSOR_URL, label: "Sponsor", Glyph: SponsorGlyph, hover: "hover:bg-[rgba(244,114,182,0.14)] hover:text-pink-400" },
];
