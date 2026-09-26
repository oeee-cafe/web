/**
 * Sentry for the painter page, in a bundle of its own.
 *
 * The offline bundle is built for Firefox 56 (vite/legacyBrowsers.ts), and
 * esbuild lowers the SDK's grammar for it like everything else's. What it
 * cannot lower is what the SDK does at runtime: `globalThis` is Firefox 65,
 * and an SDK that supports no engine older than ES2020 assumes more than
 * that. A static import into offline.js would put those assumptions in
 * front of the painter, and one ReferenceError while the module evaluates
 * would take the whole page with it -- on exactly the browser the floor is
 * kept for, with nothing on screen to explain it.
 *
 * So the page loads this as a second module script (draw_post_cucumber.jinja),
 * before offline.js. A browser that cannot run it fails this script alone,
 * and the painter goes on drawing without a reporter, which is what it had
 * before. On any browser that can, the global handlers this installs catch
 * what offline.js throws: the two share nothing but the page.
 */
import * as Sentry from "@sentry/browser";
import { forwardPainterReports, SENTRY_OPTIONS } from "../shared/sentry";

Sentry.init(SENTRY_OPTIONS);
forwardPainterReports(Sentry.captureMessage);
