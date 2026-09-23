/**
 * The replay viewer's own catalogs.
 *
 * Its labels are messages like every other string in this package -- extracted
 * by the same command, translated in the same format, reviewed in the same
 * place -- but they are compiled into a catalog of their own rather than the
 * package's.
 *
 * That is not tidiness, it is weight. The viewer ships as a standalone script
 * on every post page, and the package catalog carries the toolbox, the modals
 * and everything else -- sixty-odd messages to render seven labels. Its own
 * catalog holds seven, which is most of the difference between a 61kB viewer
 * and a 36kB one.
 */
import { msg } from "@lingui/core/macro";
import { i18n } from "@lingui/core";
// By path, not through the package's entry: that would bring the painter.
import { activateLocale } from "../utils/activateLocale";
import { messages as enMessages } from "./locales/en/messages";
import { messages as jaMessages } from "./locales/ja/messages";
import { messages as koMessages } from "./locales/ko/messages";
import { messages as zhMessages } from "./locales/zh/messages";

const catalogs = {
  en: enMessages,
  ja: jaMessages,
  ko: koMessages,
  zh: zhMessages,
};

/** Activate a locale, falling back to English for anything untranslated. */
export function setupViewerI18n(locale: string): void {
  activateLocale(i18n, catalogs, locale);
}

/**
 * The viewer's own words, in the catalogs with everything else.
 *
 * A context because "Play", "Pause" and "Seek" are ordinary words this package
 * could want elsewhere, and its catalogs derive ids from the message.
 */
export const MESSAGES = {
  play: msg({ message: "Play", context: "replay viewer" }),
  pause: msg({ message: "Pause", context: "replay viewer" }),
  rewind: msg({ message: "Rewind", context: "replay viewer" }),
  skip: msg({ message: "Skip to end", context: "replay viewer" }),
  seek: msg({ message: "Seek", context: "replay viewer" }),
  loading: msg({ message: "Loading replay\u2026", context: "replay viewer" }),
  failed: msg({
    message: "This drawing's replay could not be loaded.",
    context: "replay viewer",
  }),
};

export type Labels = Record<keyof typeof MESSAGES, string>;

/**
 * Resolve the viewer's labels for a language tag.
 *
 * The page says which; `lang` is passed by the templates that mount this. The
 * document's own attribute is the fallback rather than the source, so a host
 * that embeds the viewer somewhere else still gets a language rather than a
 * silent English.
 */
export function labelsFor(lang: string | undefined): Labels {
  setupViewerI18n((lang || document.documentElement.lang || "en").split("-")[0]);
  return Object.fromEntries(
    Object.entries(MESSAGES).map(([key, message]) => [key, i18n._(message)]),
  ) as Labels;
}
