// The site's chrome around a page, as one classic script: the toolbar
// (toolbarControls.ts), the toasts, the reaction and follow guesses, the
// quick switcher, the Draw dialog, the segmented controls' thumbs and the
// Per row slider. Built to dist-site/chrome.js and loaded by toolbar.jinja,
// which every page with any of these has, before any of them.
//
// Loading it sets nothing up. Each template keeps a one-line inline script
// where its own script used to be, calling its piece here, so each is set up
// at the same point in the page, and in the same order, as before.
//
// Those lines are also what a boosted swap re-runs. The swap brings this
// tag back with the body too, and htmx re-runs it, but a script htmx puts in
// is not waited for: it would run late, out of order with the lines after
// it. So this file does its work the first time and nothing after, and
// re-running it costs only the fetch.
import { attachDrawDialog } from "./drawDialog";
import { installJump } from "./jump";
import { installOptimistic } from "./optimistic";
import { attachPerRow } from "./perRow";
import { settleThumbs } from "./thumbs";
import { lookAtToasts } from "./toasts";
import { installToolbar, showToolbar } from "./toolbarControls";

interface Chrome {
  /** Each toolbar the page gets (toolbar.jinja). */
  toolbar(): void;
  toasts(): void;
  optimistic(): void;
  jump(): void;
  drawDialog(): void;
  thumbs(): void;
  perRow(): void;
}

const host = window as unknown as { oeeeChrome?: Chrome };

if (!host.oeeeChrome) {
  let toolbarInstalled = false;
  host.oeeeChrome = {
    toolbar() {
      showToolbar();
      if (toolbarInstalled) return;
      toolbarInstalled = true;
      installToolbar();
    },
    toasts: lookAtToasts,
    optimistic: installOptimistic,
    jump: installJump,
    drawDialog: attachDrawDialog,
    thumbs: settleThumbs,
    perRow: attachPerRow,
  };
}
