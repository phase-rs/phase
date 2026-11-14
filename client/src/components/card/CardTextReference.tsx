import { useTranslation } from "react-i18next";

import { RichLabel } from "../mana/RichLabel.tsx";

/** The entire official printed text, explicitly separate from the engine's
 * active ability. Do not split or align paragraphs to guess a translation. */
export function CardTextReference({ text }: { text?: string }) {
  const { t } = useTranslation("game");
  if (!text) return null;
  return (
    <section aria-label={t("cardTextReference.title")} className="mt-3 border-t border-white/10 pt-3 text-left">
      <h3 className="text-xs font-semibold text-cyan-200">{t("cardTextReference.title")}</h3>
      <p className="mt-1 text-xs text-slate-400">{t("cardTextReference.hint")}</p>
      <RichLabel text={text} size="xs" className="mt-2 block whitespace-pre-wrap break-words text-sm leading-relaxed text-slate-200" />
    </section>
  );
}
