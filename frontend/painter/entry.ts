import {
  mount,
  NEO_BUTTON,
  NEO_PANEL,
  NEO_PANEL_BUTTON,
  NEO_TITLEBAR,
  type PainterMode,
  type PainterHandle,
} from "neo-cucumber";
// The chrome this adapter borrows below lives in the package's stylesheet,
// which a library build keeps out of the JavaScript bundle. It is imported
// through this adapter's own file so that the utilities named here are compiled
// as well as the ones the package names.
import "./painter.css";

interface OeeePainterConfig {
  width: number;
  height: number;
  locale?: string;
  communityId?: string | null;
  parentPostId?: string | null;
  initialImageUrl?: string | null;
  submission?:
    | { kind: "post" }
    | { kind: "banner"; profileUrl: string };
  mode: PainterMode;
}

interface NativeMessage {
  type: string;
  [key: string]: unknown;
}

declare global {
  interface Window {
    webkit?: {
      messageHandlers?: {
        oeee?: { postMessage(message: NativeMessage): void };
      };
    };
    OeeeCafe?: { postMessage(message: string): void };
  }
}

function nativeAvailable(): boolean {
  return Boolean(
    window.webkit?.messageHandlers?.oeee || window.OeeeCafe?.postMessage,
  );
}

function postNative(message: NativeMessage): void {
  if (window.webkit?.messageHandlers?.oeee) {
    window.webkit.messageHandlers.oeee.postMessage(message);
  } else if (window.OeeeCafe?.postMessage) {
    window.OeeeCafe.postMessage(JSON.stringify(message));
  }
}

function blobToDataUrl(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = () => reject(reader.error ?? new Error("Failed to read PNG"));
    reader.readAsDataURL(blob);
  });
}

async function submit(
  painter: PainterHandle,
  config: OeeePainterConfig,
  startedAt: number,
  onSaved: () => void,
): Promise<void> {
  const snapshot = await painter.save();
  const form = new FormData();
  form.append("image", await blobToDataUrl(snapshot.png));
  form.append("animation", snapshot.replay);
  form.append("width", String(snapshot.width));
  form.append("height", String(snapshot.height));
  form.append("security_timer", String(startedAt));
  form.append("security_count", String(snapshot.strokeCount));

  const submission = config.submission ?? { kind: "post" as const };
  if (submission.kind === "post") {
    form.append(
      "tool",
      config.mode.kind === "two-tone" ? "cucumber" : "neo-cucumber-offline",
    );
    if (config.communityId) form.append("community_id", config.communityId);
    if (config.parentPostId) form.append("parent_post_id", config.parentPostId);
  }

  const response = await fetch(
    submission.kind === "banner" ? "/banners/draw/finish" : "/draw/finish",
    { method: "POST", body: form },
  );
  const result = await response.json();
  if (!response.ok || result?.error) {
    throw new Error(result?.error ?? `Upload failed: ${response.status}`);
  }

  // Saved: the drawing is on the server, so leaving is no longer a loss.
  onSaved();

  if (submission.kind === "banner") {
    if (nativeAvailable()) {
      postNative({
        type: "banner_complete",
        bannerId: result.banner_id,
        imageUrl: result.image_url,
      });
    } else {
      window.location.href = submission.profileUrl;
    }
  } else if (nativeAvailable()) {
    postNative({
      type: "drawing_complete",
      postId: result.post_id,
      communityId: result.community_id,
      imageUrl: result.image_url,
    });
  } else {
    window.location.href = `/posts/${result.post_id}/publish`;
  }
}

const root = document.getElementById("neo-cucumber-root");
const configElement = document.getElementById("neo-cucumber-config");
const saveButton = document.getElementById("oeee-painter-save") as HTMLButtonElement | null;

if (!root || !configElement?.textContent || !saveButton) {
  throw new Error("Oeee painter host is missing its root, configuration, or Save button");
}
const painterRoot = root;
const pageSaveButton = saveButton;

const config = JSON.parse(configElement.textContent) as OeeePainterConfig;

/** What the page calls saving, in the reader's language. */
const saveLabel = pageSaveButton.textContent?.trim() || "Save";

/**
 * Fill the bar above the painter.
 *
 * The page says what goes in it and this says what it looks like, using the
 * class names neo-cucumber publishes rather than a copy of their values kept
 * here -- the bar sits inches from the toolbox, so anything merely close to
 * NEO's chrome reads as broken rather than as different.
 *
 * It is built before the painter mounts. The panels and the opening zoom are
 * both measured from the painter's area, and a bar that appears afterwards
 * would move that area out from under them.
 */
function buildHeader(): void {
  const bar = document.getElementById("oeee-painter-header");
  if (!bar) return;

  bar.className = `${NEO_PANEL} flex shrink-0 items-center justify-between gap-[8px] px-[6px] py-[4px]`;

  const left = document.createElement("div");
  left.className = "flex min-w-0 items-center gap-[8px]";

  // The only way off this page that is not the back button.
  const home = document.createElement("a");
  home.href = bar.dataset.home || "/";
  home.className = "text-[18px] hover:opacity-70";
  home.textContent = "🥒";
  left.append(home);

  const title = document.createElement("h1");
  title.className = "m-0 truncate text-[14px] font-bold";
  title.textContent = bar.dataset.title ?? "";
  left.append(title);

  if (bar.dataset.subtitle) {
    const where = document.createElement("div");
    where.className = "shrink-0 text-[11px] opacity-70";
    where.textContent = bar.dataset.subtitle;
    left.append(where);
  }

  const right = document.createElement("div");
  right.className = "flex shrink-0 items-center gap-[6px] text-[11px]";
  if (bar.dataset.size) {
    const size = document.createElement("div");
    size.className = "tabular-nums opacity-70";
    size.textContent = bar.dataset.size;
    right.append(size);
  }

  bar.append(left, right);
}

buildHeader();

/**
 * Losing a drawing to a stray tap.
 *
 * Until now this page had no way off it but the back button, so nothing here
 * guarded against leaving. The header adds a link, which makes an accidental
 * exit a click away, so the browser now asks first -- but only once something
 * has actually been drawn. The count the painter reports at rest is the
 * baseline: it is not zero, because setting the canvas up is itself recorded.
 */
let baselineStrokes: number | null = null;
let hasUnsavedWork = false;
let leaving = false;

window.addEventListener("beforeunload", (event) => {
  if (leaving || !hasUnsavedWork) return;
  event.preventDefault();
  // Chrome shows its own wording, but only when returnValue is set.
  event.returnValue = "";
});

const startedAt = Date.now();
const painter = mount(root, {
  width: config.width,
  height: config.height,
  locale: config.locale,
  mode: config.mode,
  controls: { kind: "toolbox" },
  onChange: ({ strokeCount }) => {
    if (baselineStrokes === null) baselineStrokes = strokeCount;
    hasUnsavedWork = strokeCount > baselineStrokes;
  },
});

function movePageActionsIntoExtraToolbox(): void {
  const colorInput = painterRoot.querySelector<HTMLInputElement>(
    'input[type="color"]',
  );
  const extraTools = colorInput?.parentElement;
  const helpButton = painterRoot.querySelector<HTMLButtonElement>(
    'button[aria-label="Keyboard shortcuts"]',
  );
  if (!extraTools || !helpButton) {
    throw new Error("Oeee painter could not find the extra toolbox actions");
  }

  // neo-cucumber publishes the chrome its own panel buttons wear, so these
  // match without copying values or reading them off a rendered button. The
  // proxy activates the original hidden Help button, which keeps React's tree
  // intact when the dialog opens.
  const helpProxy = document.createElement("button");
  helpProxy.type = "button";
  helpProxy.className = NEO_PANEL_BUTTON;
  helpProxy.textContent = "Help";
  helpProxy.title = helpButton.title;
  helpProxy.setAttribute("aria-label", helpButton.getAttribute("aria-label") ?? "Help");
  helpProxy.addEventListener("click", () => helpButton.click());
  helpButton.hidden = true;
  pageSaveButton.className = NEO_PANEL_BUTTON;
  pageSaveButton.removeAttribute("style");
  extraTools.append(helpProxy, pageSaveButton);
}

void painter.ready
  .then(async () => {
    if (config.initialImageUrl) await painter.loadImage(config.initialImageUrl);
    if (config.mode.kind === "standard") movePageActionsIntoExtraToolbox();
    saveButton.disabled = false;
  })
  .catch((error) => {
    console.error(error);
    alert("Failed to start the painter.");
  });

/**
 * Asked before a drawing is sent.
 *
 * Saving is the end of this page: the drawing goes to the server and the
 * browser leaves for the post or the profile, so there is no coming back to
 * add the line that was still missing. The button that does it sits in the
 * toolbox among the drawing tools, one stray tap from whichever of them was
 * actually meant, which is exactly the mistake `beforeunload` above already
 * guards the header's link against.
 *
 * Its chrome is the painter's own, from the class names neo-cucumber exports,
 * and its words come off the button the page rendered -- the page is the only
 * thing here that knows the reader's language.
 */
function confirmSave(): Promise<boolean> {
  return new Promise((resolve) => {
    const backdrop = document.createElement("div");
    backdrop.className =
      "fixed inset-0 z-[9999] flex items-center justify-center bg-black/70";

    const panel = document.createElement("div");
    panel.className = `${NEO_PANEL} max-w-sm shadow-lg`;

    const titleBar = document.createElement("div");
    titleBar.className = `${NEO_TITLEBAR} px-[4px] text-[11px] leading-[14px]`;
    titleBar.textContent = saveLabel;

    const body = document.createElement("div");
    body.className = "p-[12px] text-center";

    const message = document.createElement("p");
    message.className = "m-0 mb-[12px]";
    message.textContent =
      pageSaveButton.dataset.confirm || "Save this drawing?";

    const actions = document.createElement("div");
    actions.className = "flex justify-center gap-[6px]";

    const cancelButton = document.createElement("button");
    cancelButton.type = "button";
    cancelButton.className = NEO_BUTTON;
    cancelButton.textContent = pageSaveButton.dataset.cancel || "Cancel";

    const confirmButton = document.createElement("button");
    confirmButton.type = "button";
    confirmButton.className = NEO_BUTTON;
    confirmButton.textContent = saveLabel;

    const close = (answer: boolean) => {
      document.removeEventListener("keydown", onKeyDown, true);
      backdrop.remove();
      resolve(answer);
    };
    function onKeyDown(event: KeyboardEvent): void {
      if (event.key !== "Escape") return;
      // The painter binds shortcuts to the window; Escape here means this
      // dialog and nothing else.
      event.stopPropagation();
      event.preventDefault();
      close(false);
    }

    document.addEventListener("keydown", onKeyDown, true);
    backdrop.addEventListener("click", () => close(false));
    panel.addEventListener("click", (event) => event.stopPropagation());
    cancelButton.addEventListener("click", () => close(false));
    confirmButton.addEventListener("click", () => close(true));

    actions.append(cancelButton, confirmButton);
    body.append(message, actions);
    panel.append(titleBar, body);
    backdrop.append(panel);
    document.body.append(backdrop);
    confirmButton.focus();
  });
}

saveButton.addEventListener("click", () => {
  saveButton.disabled = true;
  void confirmSave()
    .then((confirmed) => {
      if (!confirmed) {
        saveButton.disabled = false;
        return;
      }
      return submit(painter, config, startedAt, () => {
        leaving = true;
      }).catch((error) => {
        console.error(error);
        alert("Failed to save drawing. Please try again.");
        saveButton.disabled = false;
      });
    });
});
