// The site's own script around every page, as one classic script: the
// bridge to the apps and what answers them -- signing in, saved passwords,
// the store, Android's polyfills -- the retry after a gateway's answer, and
// the chrome: the toolbar (toolbarControls.ts), the loading bar, the Windows
// app's caption, the toasts, the reaction and follow guesses, the quick
// switcher, the live stream, the site's confirm and alert, the Draw dialog,
// the segmented controls' thumbs and the Per row slider. Built to
// dist-site/chrome.js and loaded by theme_head.jinja, in the head of every
// page with any of these, before any of them.
//
// Loading it sets nothing up. Each template keeps a one-line inline script
// where its own script used to be, calling its piece here, so each is set up
// at the same point in the page, and in the same order, as before.
//
// The head is loaded once a document -- a boosted swap keeps it -- so this
// file is fetched and run once, and the head's pieces with it. The body's
// lines are what a boosted swap re-runs, in place, as it re-ran the scripts
// they replace; each piece knows which of its work is for each page and
// which is for the document.
import { attachAlert, attachConfirm } from "./alerts";
import { installBridge, type Words } from "./bridge";
import { attachCaption } from "./caption";
import { attachDrawDialog } from "./drawDialog";
import { installGatewayRetry } from "./gatewayRetry";
import { installJump } from "./jump";
import { startLive } from "./live";
import { installLoadingBar } from "./loadingBar";
import { installOptimistic } from "./optimistic";
import { attachPerRow } from "./perRow";
import { installPolyfills } from "./polyfills";
import { installSavedPassword } from "./savedPassword";
import { installSignIn } from "./signIn";
import { installStore } from "./store";
import { settleThumbs } from "./thumbs";
import { lookAtToasts } from "./toasts";
import { installToolbar, showToolbar } from "./toolbarControls";

interface Chrome {
  // The head's, once a document (theme_head.jinja and what it includes).
  bridge(words: Words): void;
  signIn(steamFailed: string, ok: string): void;
  savedPassword(): void;
  store(): void;
  polyfills(): void;
  gatewayRetry(): void;
  // The body's, each again on a boosted swap.
  /** Each toolbar the page gets (toolbar.jinja). */
  toolbar(): void;
  loadingBar(): void;
  caption(): void;
  toasts(): void;
  optimistic(): void;
  jump(): void;
  drawDialog(): void;
  thumbs(): void;
  perRow(): void;
  live(closeLabel: string, newDrawings: string): void;
  confirmDialog(): void;
  alertDialog(): void;
}

const host = window as unknown as { oeeeChrome?: Chrome };

if (!host.oeeeChrome) {
  let toolbarInstalled = false;
  host.oeeeChrome = {
    bridge: installBridge,
    signIn: installSignIn,
    savedPassword: installSavedPassword,
    store: installStore,
    polyfills: installPolyfills,
    gatewayRetry: installGatewayRetry,
    toolbar() {
      showToolbar();
      if (toolbarInstalled) return;
      toolbarInstalled = true;
      installToolbar();
    },
    loadingBar: installLoadingBar,
    caption: attachCaption,
    toasts: lookAtToasts,
    optimistic: installOptimistic,
    jump: installJump,
    drawDialog: attachDrawDialog,
    thumbs: settleThumbs,
    perRow: attachPerRow,
    live: startLive,
    confirmDialog: attachConfirm,
    alertDialog: attachAlert,
  };
}
