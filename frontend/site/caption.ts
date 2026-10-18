// The window's own controls in the Windows app (app_caption.jinja) --
// minimise, maximise or restore, close -- at the right end of the toolbar,
// where a title bar would have put them above it. Each asks the app on the
// bridge (a `window` message, app_bridge.jinja), which alone can move a
// window; close goes the way the system's button went, so a page holding a
// drawing is still asked before it is left (close_guard.rs in
// oeee-cafe-desktop).
//
// What the app knows and the page does not, it says: whether the window is
// maximised, and whether the pointer is on the maximise button or pressing it
// -- Windows offers its Snap Layouts only over what it knows for a maximise
// button, so the app keeps a stand-in of its own over this one, which the
// pointer reaches instead (snap.rs), and the page says where the button is
// for it (a `caption` message). Both come as oeeeApp.caption({maximized,
// pointer}).
//
// A boosted swap draws the toolbar, and these, anew: each new set is wired
// and shown as the app last said; the app's half is listened for once.

interface CaptionApp {
  post(type: string, data: Record<string, unknown>, now?: boolean): unknown;
  caption?: (told: { maximized?: boolean; pointer?: string } | null) => void;
}

type CaptionWindow = Window & typeof globalThis & { oeeeApp?: CaptionApp };

// What the app last said, kept across a boosted swap.
const state = { maximized: false, pointer: "" };
let listening = false;

function show(): void {
  const current = document.querySelector(".nav-bar .oeee-caption");
  if (!current) return;
  const maximize = current.querySelector<HTMLElement>(".is-maximize")!;
  const name = state.maximized ? "restore" : "maximize";
  const label = current.getAttribute("data-" + name + "-label")!;
  maximize.setAttribute("data-caption", name);
  maximize.title = label;
  maximize.setAttribute("aria-label", label);
  maximize.firstChild!.textContent = state.maximized ? "" : "";
  maximize.classList.toggle("is-hot", state.pointer === "hot");
  maximize.classList.toggle("is-pressed", state.pointer === "pressed");
}

// Where the maximise button is, in the window's pixels, for the app's
// stand-in -- said when it may have moved, and only when it has.
let reporting = false;
function report(app: CaptionApp): void {
  if (reporting) return;
  reporting = true;
  requestAnimationFrame(() => {
    reporting = false;
    const button = document.querySelector(".nav-bar .oeee-caption .is-maximize");
    const r = button && button.getBoundingClientRect();
    const scale = window.devicePixelRatio || 1;
    const place =
      r && r.width > 0 && r.height > 0
        ? {
            x: Math.round(r.left * scale),
            y: Math.round(r.top * scale),
            width: Math.round(r.right * scale) - Math.round(r.left * scale),
            height: Math.round(r.bottom * scale) - Math.round(r.top * scale),
          }
        : null;
    app.post("caption", { place });
  });
}

/** Wires the caption this toolbar has, in the Windows app only. */
export function attachCaption(): void {
  const root = document.documentElement;
  const app = (window as CaptionWindow).oeeeApp;
  if (root.getAttribute("data-app") !== "windows" || !app) return;
  const box = document.querySelector(".nav-bar .oeee-caption");
  if (!box) return;

  // A press here is the button's, not the start of a drag of the window,
  // which the toolbar around it is (data-window-drag).
  box.addEventListener("mousedown", (event) => event.stopPropagation());
  box.addEventListener("click", (event) => {
    const target = event.target as Element | null;
    const button = target && target.closest ? target.closest("[data-caption]") : null;
    if (!button) return;
    const action = button.classList.contains("is-close")
      ? "close"
      : button.classList.contains("is-maximize")
        ? "maximize"
        : "minimize";
    app.post("window", { action }, true);
  });

  show();
  report(app);
  if (listening) return;
  listening = true;
  app.caption = (told) => {
    if (!told) return;
    if ("maximized" in told) state.maximized = !!told.maximized;
    if ("pointer" in told) state.pointer = told.pointer || "";
    show();
  };
  window.addEventListener("resize", () => report(app));
  // A window in the background greys its controls.
  function focused(): void {
    root.toggleAttribute("data-window-inactive", !document.hasFocus());
  }
  window.addEventListener("focus", focused);
  window.addEventListener("blur", focused);
  focused();
}
