// New drawing, from the toolbar's Draw button: the canvas size, asked for
// first (draw_dialog.jinja). The button stays a form of its own at 300 × 300,
// which is what a press does without script. With it, the press opens the
// dialog instead (`oeeeDrawDialog`), as do the `new-drawing` command and its
// key.
//
// A boosted swap brings a new dialog with the body. Its own listeners are
// added to it each time; the document's, once.

type DrawDialogWindow = Window & typeof globalThis & { oeeeDrawDialog?: { open(): boolean; close(): void } };

const page = window as DrawDialogWindow;

let opener: Element | null = null;

function dialog(): HTMLElement | null {
  return document.getElementById("draw-dialog");
}

function open(): boolean {
  const modal = dialog();
  if (!modal) return false;
  opener = document.activeElement;
  modal.style.display = "flex";
  // The heading, not the first field: on iOS a focused <select> opens its
  // picker, so the width wheel would come up with the dialog.
  document.getElementById("draw-dialog-title")!.focus();
  return true;
}

function close(): void {
  const modal = dialog();
  if (!modal || modal.style.display === "none") return;
  modal.style.display = "none";
  const back = opener as HTMLElement | null;
  if (back && document.body.contains(back)) back.focus();
  opener = null;
}

/** Wires the dialog the page has now, and the first time, the Draw button. */
export function attachDrawDialog(): void {
  const modal = dialog();
  if (modal) {
    modal.querySelector("[data-draw-dialog-cancel]")!.addEventListener("click", close);
    modal.addEventListener("mousedown", (event) => {
      // Pressing the dimmed page is a Cancel.
      if (event.target === modal) close();
    });
    modal.addEventListener("keydown", (event) => {
      if (event.key === "Escape" || event.key === "Esc") {
        event.preventDefault();
        close();
      }
    });
  }

  if (page.oeeeDrawDialog) return;
  page.oeeeDrawDialog = { open, close };
  document.addEventListener("click", (event) => {
    if (event.defaultPrevented || event.button !== 0) return;
    const target = event.target as Element | null;
    const button = target && target.closest ? target.closest("#nav-draw-button") : null;
    if (button && open()) event.preventDefault();
  });
}
