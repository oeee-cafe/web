/**
 * How the painter tells the page something went wrong that it recovered from.
 *
 * A recovery hides the failure it recovers from, and a hidden failure is one
 * nobody will ever find the cause of. So the painter says so, but not to
 * Sentry directly: on /draw the reporter is a script of its own
 * (frontend/painter/sentry.ts) that the painter must not import, and a
 * library has no business choosing a page's reporter anyway. It raises an
 * event on the window instead, and whichever reporter the page loaded passes
 * it on (frontend/shared/sentry.ts). With none loaded, it goes nowhere.
 *
 * Kept free of imports so the reporter's bundle can take this file alone.
 */

export const PAINTER_REPORT_EVENT = "neo-cucumber:report";

export interface PainterReport {
  /** One line, the same every time, so that Sentry groups them. */
  message: string;
  /** What was known when it happened. */
  details: Record<string, unknown>;
}

export function reportFromPainter(report: PainterReport): void {
  window.dispatchEvent(
    new CustomEvent<PainterReport>(PAINTER_REPORT_EVENT, { detail: report })
  );
}
