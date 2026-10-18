// A toast (toasts.jinja) that is not an error goes after a while -- counted
// from when the page is in view -- sliding down as it came; the pointer
// resting on one holds it, so it is not taken away mid-read. Only the three
// newest are kept, so a run of clicks does not build a tower. Looked for on
// load and after every htmx swap -- partials included, since htmx fires the
// swap event once all of a response's tasks are done -- and a toast is timed
// once, however many swaps see it. A swap of the whole body runs
// toasts.jinja's line again, and only needs the look.

type ToastsWindow = Window & typeof globalThis & { oeeeToasts?: { look(): void } };

const page = window as ToastsWindow;

const SHOWN_MS = 5000;
const KEPT = 3;

function leave(toast: HTMLElement): void {
  if (!toast.parentNode) return;
  toast.className += " is-leaving";
  setTimeout(() => {
    if (toast.parentNode) toast.parentNode.removeChild(toast);
  }, 200);
}

// The time starts when the page is being looked at: a toast that arrives in
// a tab in the background -- a live notification, most often (live.jinja) --
// waits there to be read rather than going before anyone was there to see it.
function time(toast: HTMLElement): void {
  let timer: ReturnType<typeof setTimeout> | undefined;
  function start(): void {
    clearTimeout(timer);
    timer = setTimeout(() => leave(toast), SHOWN_MS);
  }
  function whenSeen(): void {
    if (document.hidden) return;
    document.removeEventListener("visibilitychange", whenSeen);
    start();
  }
  toast.addEventListener("mouseenter", () => clearTimeout(timer));
  toast.addEventListener("mouseleave", start);
  if (document.hidden) document.addEventListener("visibilitychange", whenSeen);
  else start();
}

function look(): void {
  const stack = document.getElementById("toasts");
  if (!stack) return;
  const toasts = stack.querySelectorAll<HTMLElement>(".ds-notice");
  for (let i = 0; i < toasts.length; i++) {
    const toast = toasts[i];
    if (i < toasts.length - KEPT) {
      leave(toast);
      continue;
    }
    if (toast.getAttribute("data-timed")) continue;
    toast.setAttribute("data-timed", "1");
    if (toast.className.indexOf("ds-toast") < 0) toast.className += " ds-toast";
    if (toast.className.indexOf("ds-notice-error") < 0) time(toast);
  }
}

/** Times the toasts on the page, and the first time, every toast after. */
export function lookAtToasts(): void {
  look();
  if (page.oeeeToasts) return;
  // For a toast put up by script rather than by a swap (live.jinja).
  page.oeeeToasts = { look };
  document.addEventListener("htmx:after:swap", look);
}
