// What the pages' controls do when pressed, typed into or sent, declared in
// their markup as data attributes rather than as onclick, oninput, onsubmit,
// onchange or hx-on code. Listened for once, on the document, from the head
// (theme_head.jinja), so a control htmx swaps in later -- a toast's close
// button, a report's outcome -- answers as one in the page did, and on every
// page with the site's chrome, the painters' included.
//
//   data-action="<name>"        a press; the names are ACTIONS below
//   data-orientation-size       on a New drawing form: portrait or landscape
//                               set its width and height as it is sent
//   data-no-submit              on a form that is only for show
//   data-confirms-slug="<id>"   on a field, with data-slug: the button <id>
//                               is enabled once the slug is typed exactly
//   data-slug-preview="<id>"    on a field: <id> shows where it will live
//   data-submit-on-change       on a select: choosing sends its form
//   data-network-error          on an htmx form: said in the site's alert
//                               when the request never reached the server
//   data-reset-after-request    on an htmx form: emptied once it was taken

import {
  cancelReply,
  hideCommunityDrawModal,
  hideProfileReportModal,
  modalToggle,
  replyToComment,
  setOrientationDimensions,
  sharePost,
  showCommunityDrawModal,
  showProfileReportModal,
  showReportModal,
  toggleSessionDialog,
} from "./pages/modals";

type ActionsWindow = Window & typeof globalThis & { dsAlert?: (text: string) => void };

/** What each data-action does, given the control that was pressed. */
const ACTIONS: Record<string, (control: HTMLElement) => void> = {
  // The post page (post_view.jinja, comments_macro.jinja).
  "post-draw-open": () => modalToggle("post-draw-modal", true),
  "post-draw-close": () => modalToggle("post-draw-modal", false),
  "post-report-open": showReportModal,
  "post-report-close": () => modalToggle("report-modal", false),
  "reply-to-comment": replyToComment,
  "cancel-reply": cancelReply,
  "share-post": sharePost,
  // A community's New drawing (community.jinja).
  "community-draw-open": showCommunityDrawModal,
  "community-draw-close": hideCommunityDrawModal,
  // New session (collaborate_create_dialog.jinja), from the lobby and a
  // community.
  "session-dialog-open": () => toggleSessionDialog(true),
  "session-dialog-close": () => toggleSessionDialog(false),
  // The profile's report, from the menu it sits in (profile.jinja).
  "profile-report-open": (control) => {
    const menu = control.closest("details");
    if (menu) menu.removeAttribute("open");
    showProfileReportModal();
  },
  "profile-report-close": hideProfileReportModal,
  // A report's outcome, swapped into either report modal (report_result.jinja).
  "close-report": (control) => {
    const modal = control.closest<HTMLElement>("[id$=report-modal]");
    if (modal) modal.style.display = "none";
  },
  // A notice's close button (notice_close.jinja, and web::htmx's by hand):
  // the notice goes, and a list left with nothing in it is emptied outright,
  // so `:empty` takes the list away too.
  "dismiss-notice": (control) => {
    const notice = control.parentNode as HTMLElement;
    const list = notice.parentNode as HTMLElement;
    list.removeChild(notice);
    if (!list.querySelector(".ds-notice")) list.innerHTML = "";
  },
};

function closestTo<T extends Element = HTMLElement>(target: EventTarget | null, selector: string): T | null {
  const element = target as Element | null;
  return element && element.closest ? element.closest<T>(selector) : null;
}

let installed = false;

/** Listens for every declared control, once for the document. */
export function installActions(): void {
  if (installed) return;
  installed = true;

  document.addEventListener("click", (event) => {
    const control = closestTo(event.target, "[data-action]");
    if (!control) return;
    const name = control.getAttribute("data-action")!;
    const act = Object.prototype.hasOwnProperty.call(ACTIONS, name) ? ACTIONS[name] : undefined;
    if (act) act(control);
  });

  document.addEventListener("submit", (event) => {
    const form = event.target as HTMLFormElement;
    if (!form || !form.hasAttribute) return;
    if (form.hasAttribute("data-no-submit")) event.preventDefault();
    else if (form.hasAttribute("data-orientation-size")) setOrientationDimensions(form);
  });

  document.addEventListener("input", (event) => {
    const field = event.target as HTMLInputElement;
    if (!field || !field.getAttribute) return;
    const confirms = field.getAttribute("data-confirms-slug");
    if (confirms) {
      const button = document.getElementById(confirms) as HTMLButtonElement | null;
      if (button) button.disabled = field.value !== field.getAttribute("data-slug");
    }
    const preview = field.getAttribute("data-slug-preview");
    if (preview) {
      const where = document.getElementById(preview);
      if (where) where.textContent = "/@" + field.value;
    }
  });

  document.addEventListener("change", (event) => {
    const select = event.target as HTMLSelectElement;
    if (select && select.hasAttribute && select.hasAttribute("data-submit-on-change") && select.form) {
      select.form.submit();
    }
  });

  // htmx's own events, on the form that asked.
  document.addEventListener("htmx:error", (event) => {
    const form = closestTo(event.target, "[data-network-error]");
    const ctx = (event as CustomEvent<{ ctx?: { response?: unknown } }>).detail?.ctx;
    if (!form || (ctx && ctx.response)) return;
    (window as ActionsWindow).dsAlert!(form.getAttribute("data-network-error")!);
  });
  document.addEventListener("htmx:after:request", (event) => {
    const form = closestTo<HTMLFormElement>(event.target, "[data-reset-after-request]");
    const ctx = (event as CustomEvent<{ ctx?: { response?: { status: number } } }>).detail?.ctx;
    if (form && ctx && ctx.response && ctx.response.status < 400) form.reset();
  });
}
