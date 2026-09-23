import { i18n } from "@lingui/core";
import { activateLocale } from "neo-cucumber";
import { messages as enMessages } from "./locales/en/messages";
import { messages as jaMessages } from "./locales/ja/messages";
import { messages as koMessages } from "./locales/ko/messages";
import { messages as zhMessages } from "./locales/zh/messages";

/**
 * This host's own catalogs: the lobby, the chat and the session's modals. The
 * painter it mounts loads its own into the same instance when it mounts.
 */
const catalogs = {
  en: enMessages,
  ja: jaMessages,
  ko: koMessages,
  zh: zhMessages,
};

/** Activate this host's words in a locale; English when it has none. */
export const setupI18n = (locale: string) =>
  activateLocale(i18n, catalogs, locale);

/**
 * The signed-in reader's chosen language, from the site's auth endpoint, or
 * null for a guest, a reader who has not chosen, or a request that failed --
 * all of which mean the same thing to the caller: keep the language it has.
 *
 * Here rather than in the package because `/api/auth` is this site's, and the
 * package does not know what site it has been mounted on.
 */
export const fetchPreferredLocale = async (): Promise<string | null> => {
  try {
    const response = await fetch("/api/auth", { credentials: "include" });
    if (!response.ok) return null;
    const auth = await response.json();
    return auth.preferred_locale || null;
  } catch (error) {
    console.error("Failed to fetch preferred locale:", error);
    return null;
  }
};
