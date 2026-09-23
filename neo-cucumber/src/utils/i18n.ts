import { i18n } from "@lingui/core";
import { activateLocale } from "./activateLocale";
import { messages as enMessages } from "../locales/en/messages";
import { messages as jaMessages } from "../locales/ja/messages";
import { messages as koMessages } from "../locales/ko/messages";
import { messages as zhMessages } from "../locales/zh/messages";

/** The painter's own catalogs: the toolbox, its windows and its modals. */
const catalogs = {
  en: enMessages,
  ja: jaMessages,
  ko: koMessages,
  zh: zhMessages,
};

/** Activate the painter's words in a locale; English when it has none. */
export const setupI18n = (locale: string) =>
  activateLocale(i18n, catalogs, locale);
