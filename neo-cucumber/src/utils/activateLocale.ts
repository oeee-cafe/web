import type { I18n, Messages } from "@lingui/core";

/**
 * A bundle's compiled catalogs, by language. English is required because it is
 * what everything else falls back to.
 */
export type Catalogs = { en: Messages } & Partial<Record<string, Messages>>;

/**
 * Load a bundle's catalog for a locale and make it the active one, falling
 * back to English for any language the bundle has no catalog for.
 *
 * Every bundle here does this -- the painter, the collaborative host and the
 * replay viewer -- each with catalogs of its own, since the viewer's seven
 * labels should not cost a page the painter's sixty. What they share is only
 * this step, and it is the step that used to disagree: one copy activated an
 * unsupported tag under its own name with English loaded into it, the others
 * activated English. An unsupported language activates `en` everywhere now.
 *
 * The instance is passed rather than imported, so that the catalogs land in
 * whichever `i18n` the caller renders with. In this repository there is only
 * one; a host consuming a built copy of the package would have two, and
 * activating the package's own would change nothing it displays.
 *
 * Loading merges rather than replaces, which is what lets the collaborative
 * host's catalog and the painter's share one instance: each call adds its
 * messages to the language and leaves the other's in place.
 */
export function activateLocale(
  i18n: I18n,
  catalogs: Catalogs,
  locale: string,
): void {
  // Own properties only: "constructor" is not a language.
  const catalog = Object.prototype.hasOwnProperty.call(catalogs, locale)
    ? catalogs[locale]
    : undefined;
  const language = catalog ? locale : "en";
  i18n.load(language, catalog ?? catalogs.en);
  i18n.activate(language);
}
