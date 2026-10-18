// The site's own confirm and alert (confirm_dialog.jinja), in place of the
// browser's: confirm() is titled with the site's address, drawn by the
// browser, and worded in the browser's language, and nothing else on a page
// gave it away as a web page so plainly; alert() the Android app draws as the
// web view's own box, titled "oeee.cafe says", and the Apple and Windows apps
// do not draw at all. The site calls neither (frontend/shared/siteDialog.ts).
//
// htmx fires `htmx:confirm` before a confirmed request; cancelling that event
// hands the decision to us, and `issueRequest` / `dropRequest` give it back.
// The action button is named for what it does -- the control's own words,
// "Delete" or "Remove member" -- rather than OK, and Cancel is the one that
// has focus, so Return never destroys anything by itself. A control with no
// words of its own, or whose yes destroys nothing, says so:
// `data-confirm-action` names the button, and `data-confirm-tone="plain"`
// makes it the primary one rather than the red one, without the warning.
//
// A boosted swap brings both dialogs back with the body. Their buttons are
// wired each time; what asks through them -- htmx's question, dsConfirm and
// dsAlert -- is set up once and finds them by id when it asks.

interface ConfirmDetail {
  ctx?: { confirm: string };
  issueRequest?: () => void;
  dropRequest?: () => void;
}

type AlertsWindow = Window &
  typeof globalThis & {
    oeeeApp?: { connected(): boolean; feel(name: string): void };
    dsConfirm?: (text: string, action?: string, tone?: string) => Promise<boolean>;
    dsAlert?: (text: string) => void;
  };

const page = window as AlertsWindow;

function byId(id: string): HTMLElement {
  return document.getElementById(id)!;
}

// The confirm's parts, as the page has them now.
function confirmParts() {
  const dialog = byId("ds-confirm");
  return {
    dialog,
    message: byId("ds-confirm-message"),
    cancel: dialog.querySelector<HTMLButtonElement>("[data-confirm-cancel]")!,
    ok: dialog.querySelector<HTMLButtonElement>("[data-confirm-ok]")!,
  };
}

let pending: ((confirmed: boolean) => void) | null = null;
let confirmOpener: Element | null = null;
// The OK button's own words, as the server wrote them: Delete.
let fallbackLabel = "";

// What the control says it does. A form speaks through its submit button; an
// icon button through its label.
function labelFor(elt: Element): string {
  const control =
    elt.tagName === "FORM"
      ? elt.querySelector('button[type="submit"], button:not([type]), input[type="submit"]')
      : elt;
  if (!control) return fallbackLabel;
  let text = control.tagName === "INPUT" ? (control as HTMLInputElement).value : control.textContent;
  text = (text || "").replace(/\s+/g, " ").trim();
  return text || control.getAttribute("aria-label") || control.getAttribute("title") || fallbackLabel;
}

// A question whose yes destroys something is felt as a warning, as an app's
// own destructive alert is (theme_head.jinja).
function warn(): void {
  if (page.oeeeApp && page.oeeeApp.connected()) page.oeeeApp.feel("warning");
}

function closeConfirm(confirmed: boolean): void {
  const decide = pending;
  pending = null;
  byId("ds-confirm").style.display = "none";
  const back = confirmOpener as HTMLElement | null;
  if (back && document.body.contains(back)) back.focus();
  confirmOpener = null;
  if (decide) decide(confirmed);
}

// Puts the question up: `action` on the yes button, red unless `plain`.
function ask(text: string, action: string, plain: boolean): void {
  const { dialog, message, cancel, ok } = confirmParts();
  confirmOpener = document.activeElement;
  message.textContent = text;
  ok.textContent = action;
  ok.classList.toggle("ds-button-danger", !plain);
  ok.classList.toggle("ds-button-primary", plain);
  dialog.style.display = "flex";
  cancel.focus();
  if (!plain) warn();
}

let confirmInstalled = false;

/** Wires the confirm the page has now, and the first time, what asks through it. */
export function attachConfirm(): void {
  const { dialog, cancel, ok } = confirmParts();
  fallbackLabel = ok.textContent || "";
  cancel.addEventListener("click", () => closeConfirm(false));
  ok.addEventListener("click", () => closeConfirm(true));
  dialog.addEventListener("mousedown", (event) => {
    // Pressing the dimmed page is a Cancel, as it is for a sheet.
    if (event.target === dialog) closeConfirm(false);
  });
  dialog.addEventListener("keydown", (event) => {
    if (event.key === "Escape" || event.key === "Esc") {
      event.preventDefault();
      closeConfirm(false);
    } else if (event.key === "Tab") {
      // Two buttons, and the keyboard stays between them.
      event.preventDefault();
      (document.activeElement === cancel ? ok : cancel).focus();
    }
  });

  if (confirmInstalled) return;
  confirmInstalled = true;
  document.addEventListener("htmx:confirm", (event) => {
    const detail = (event as CustomEvent<ConfirmDetail>).detail;
    if (!detail || !detail.ctx || !detail.issueRequest) return;
    event.preventDefault();
    if (pending) pending(false);
    pending = (confirmed) => {
      if (confirmed) detail.issueRequest!();
      else detail.dropRequest!();
    };
    const target = event.target as Element;
    ask(
      detail.ctx.confirm,
      target.getAttribute("data-confirm-action") || labelFor(target),
      target.getAttribute("data-confirm-tone") === "plain",
    );
  });

  // The same question from a script of the site's own, in place of the
  // browser's confirm(): the page's question, and what saying yes does
  // (`action`, or Delete), with Cancel focused. Resolves to whether it was
  // yes. A question still open when another is asked is answered no. `tone`
  // "plain" is a question that destroys nothing, whose yes is the primary
  // button rather than the red one.
  page.dsConfirm = (text, action, tone) =>
    new Promise<boolean>((resolve) => {
      if (pending) pending(false);
      pending = resolve;
      ask(text, action || fallbackLabel, tone === "plain");
    });
}

let alertOpener: Element | null = null;
let alertInstalled = false;

function closeAlert(): void {
  byId("ds-alert").style.display = "none";
  const back = alertOpener as HTMLElement | null;
  if (back && document.body.contains(back)) back.focus();
  alertOpener = null;
}

/**
 * Wires the alert the page has now, and the first time, `dsAlert(message)`:
 * something the page has to say, and OK.
 */
export function attachAlert(): void {
  const dialog = byId("ds-alert");
  const ok = dialog.querySelector<HTMLButtonElement>("[data-alert-ok]")!;
  ok.addEventListener("click", closeAlert);
  dialog.addEventListener("mousedown", (event) => {
    if (event.target === dialog) closeAlert();
  });
  dialog.addEventListener("keydown", (event) => {
    if (event.key === "Escape" || event.key === "Esc") {
      event.preventDefault();
      closeAlert();
    } else if (event.key === "Tab") {
      // One button: the keyboard stays on it.
      event.preventDefault();
    }
  });

  if (alertInstalled) return;
  alertInstalled = true;
  page.dsAlert = (text) => {
    const shown = byId("ds-alert");
    if (shown.style.display === "none") alertOpener = document.activeElement;
    byId("ds-alert-message").textContent = text;
    shown.style.display = "flex";
    shown.querySelector<HTMLButtonElement>("[data-alert-ok]")!.focus();
  };
}
