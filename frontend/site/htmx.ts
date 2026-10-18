// htmx and the two of its own extensions the site uses, as one classic script
// in base.jinja's head, built to dist-site/htmx.js. preload fetches a boosted
// link's page at the press, before the click lands -- the toolbar's sections
// and every in-place pill (pill_macro.jinja, which asks for the hover as
// well). history-cache keeps the last ten pages left, so going back puts one
// up at once, scrolled where it was, instead of fetching it again (the
// skeleton is taken down before it is kept, toolbar.jinja).
//
// Bundled rather than fetched from a CDN, so every page's navigation depends
// on this server alone, and so it is lowered for Firefox 56 like the rest.
//
// The ES build assigns window.htmx itself, and imports run in order, so it is
// there before the extensions, which register through that global. The
// page's inline scripts and the apps reach it the same way.
import "htmx.org/dist/htmx.esm.js";
import "htmx.org/dist/ext/hx-preload.js";
import "htmx.org/dist/ext/hx-history-cache.js";
