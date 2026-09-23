import {
  mount,
  NEO_BUTTON,
  NEO_PANEL,
  NEO_PANEL_BUTTON,
  NEO_TITLEBAR,
  type PainterMode,
  type PainterHandle,
} from "neo-cucumber";
import { offerPainterToApp } from "../shared/appBridge";
import { say } from "../shared/siteDialog";
import {
  blobToArrayBuffer,
  deleteLocalDraft,
  GUEST_OWNER,
  newDraftId,
  putLocalDraft,
  type LocalDraft,
} from "../shared/localDrafts";
import {
  blobToDataUrl,
  downloadPng,
  postDrawing,
  uploadLocalDraft,
} from "../shared/drawingUpload";
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
  communityName?: string | null;
  parentPostId?: string | null;
  /** The account drawing, or null for a guest; see `LocalDraft.owner`. */
  userId?: string | null;
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

// The hand-off to the native painter screens of the apps before they became
// web views (oeee-cafe-apple before e1846e7, oeee-cafe-android before
// 43ee094), which opened this page in a web view of their own and took over
// once it was saved. Builds of those are still installed; nothing current
// registers either name, so for everyone else this is a plain page load.
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

/** The page's words for what saving says, rendered by the server. */
interface PainterWords {
  guestSaveConfirm: string;
  guestSavedTitle: string;
  guestSaved: string;
  localSaveFailed: string;
  uploadFailedKept: string;
  uploadFailedLost: string;
  signIn: string;
  signUp: string;
  downloadPng: string;
  done: string;
  keepDrawing: string;
}

/** Where a guest's drawing waits for them, and where signing in returns to. */
const DRAFTS_PATH = "/posts/drafts";

/**
 * One id for every save this page makes. A drawing saved again -- after a
 * failed upload, or by a guest who kept drawing -- replaces its earlier copy
 * in this browser rather than sitting beside it.
 */
const draftId = newDraftId();

async function submitBanner(
  painter: PainterHandle,
  profileUrl: string,
  startedAt: number,
  onSaved: () => void,
): Promise<void> {
  const snapshot = await painter.save();
  const form = new FormData();
  form.append("image", await blobToDataUrl(snapshot.png));
  form.append("animation", snapshot.replay);
  form.append("width", String(snapshot.width));
  form.append("height", String(snapshot.height));
  form.append("paint_duration_ms", String(Date.now() - startedAt));
  form.append("security_count", String(snapshot.strokeCount));

  const result = await postDrawing<{ banner_id: string; image_url: string }>(
    "/banners/draw/finish",
    form,
  );
  onSaved();
  if (nativeAvailable()) {
    postNative({
      type: "banner_complete",
      bannerId: result.banner_id,
      imageUrl: result.image_url,
    });
  } else {
    window.location.href = profileUrl;
  }
}

/**
 * Save a post: into this browser first, then to the server for someone signed
 * in. What happens next is the caller's -- `kept` says whether this browser
 * has it, `posted` whether the server does.
 */
async function submitPost(
  painter: PainterHandle,
  config: OeeePainterConfig,
  startedAt: number,
  onPosted: () => void,
): Promise<{ draft: LocalDraft; kept: boolean; posted: boolean }> {
  const snapshot = await painter.save();
  const draft: LocalDraft = {
    id: draftId,
    owner: config.userId ?? GUEST_OWNER,
    savedAt: Date.now(),
    png: await blobToArrayBuffer(snapshot.png),
    replay: await blobToArrayBuffer(snapshot.replay),
    width: snapshot.width,
    height: snapshot.height,
    tool: config.mode.kind === "two-tone" ? "cucumber" : "neo-cucumber-offline",
    paintDurationMs: Date.now() - startedAt,
    strokeCount: snapshot.strokeCount,
    communityId: config.communityId ?? null,
    communityName: config.communityName ?? null,
    parentPostId: config.parentPostId ?? null,
  };

  let kept = false;
  try {
    await putLocalDraft(draft);
    kept = true;
  } catch (error) {
    console.error("Could not keep the drawing in this browser", error);
  }

  if (!config.userId) return { draft, kept, posted: false };

  const result = await uploadLocalDraft(draft).catch((error: unknown) => {
    console.error(error);
    return null;
  });
  if (!result) return { draft, kept, posted: false };

  // On the server now; the copy here would only offer to post it again.
  if (kept) await deleteLocalDraft(draft.id).catch(console.error);

  onPosted();
  if (nativeAvailable()) {
    postNative({
      type: "drawing_complete",
      postId: result.post_id,
      communityId: result.community_id,
      imageUrl: result.image_url,
    });
  } else {
    window.location.href = `/posts/${result.post_id}/publish`;
  }
  return { draft, kept, posted: true };
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
const wordsElement = document.getElementById("oeee-painter-words");
const words = JSON.parse(wordsElement?.textContent || "{}") as Partial<PainterWords>;

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

  // A guest is told up front where their drawing will go: this browser. It
  // gives way to the title on a narrow phone, and says the rest on hover.
  if (bar.dataset.notice) {
    const notice = document.createElement("a");
    notice.href = bar.dataset.noticeHref || "/login";
    notice.className = "min-w-0 truncate text-[11px] underline";
    notice.textContent = bar.dataset.notice;
    notice.title = bar.dataset.notice;
    left.append(notice);
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
let latestStrokes = 0;
let hasUnsavedWork = false;
let leaving = false;

/** What is on the canvas now is kept somewhere; leaving loses nothing yet. */
function markKept(): void {
  baselineStrokes = latestStrokes;
  hasUnsavedWork = false;
}

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
    latestStrokes = strokeCount;
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
    offerPainterToApp(painter);
  })
  .catch((error) => {
    console.error(error);
    say("Failed to start the painter.");
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
interface DialogChoice {
  key: string;
  label: string;
}

/**
 * A question in the painter's own chrome, answered with the key of the
 * button pressed, or null for Escape or a click outside it.
 */
function askInPainter(
  title: string,
  text: string,
  choices: DialogChoice[],
): Promise<string | null> {
  return new Promise((resolve) => {
    const backdrop = document.createElement("div");
    backdrop.className =
      "fixed inset-0 z-[9999] flex items-center justify-center bg-black/70";

    const panel = document.createElement("div");
    panel.className = `${NEO_PANEL} max-w-sm shadow-lg`;

    const titleBar = document.createElement("div");
    titleBar.className = `${NEO_TITLEBAR} px-[4px] text-[11px] leading-[14px]`;
    titleBar.textContent = title;

    const body = document.createElement("div");
    body.className = "p-[12px] text-center";

    const message = document.createElement("p");
    message.className = "m-0 mb-[12px]";
    message.textContent = text;

    const actions = document.createElement("div");
    actions.className = "flex flex-wrap justify-center gap-[6px]";

    const close = (answer: string | null) => {
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
      close(null);
    }

    const buttons = choices.map((choice) => {
      const button = document.createElement("button");
      button.type = "button";
      button.className = NEO_BUTTON;
      button.textContent = choice.label;
      button.addEventListener("click", () => close(choice.key));
      return button;
    });

    document.addEventListener("keydown", onKeyDown, true);
    backdrop.addEventListener("click", () => close(null));
    panel.addEventListener("click", (event) => event.stopPropagation());

    actions.append(...buttons);
    body.append(message, actions);
    panel.append(titleBar, body);
    backdrop.append(panel);
    document.body.append(backdrop);
    buttons[buttons.length - 1]?.focus();
  });
}

/**
 * Asked before a drawing is sent.
 *
 * Saving is the end of this page for someone signed in: the drawing goes to
 * the server and the browser leaves for the post or the profile, so there is
 * no coming back to add the line that was still missing. The button that
 * does it sits in the toolbox among the drawing tools, one stray tap from
 * whichever of them was actually meant, which is exactly the mistake
 * `beforeunload` above already guards the header's link against.
 *
 * Its chrome is the painter's own, from the class names neo-cucumber exports,
 * and its words come off the button the page rendered -- the page is the only
 * thing here that knows the reader's language.
 */
async function confirmSave(): Promise<boolean> {
  const guest = !config.userId && config.submission?.kind !== "banner";
  const question = guest
    ? words.guestSaveConfirm || "Save this drawing in this browser?"
    : pageSaveButton.dataset.confirm || "Save this drawing?";
  const answer = await askInPainter(saveLabel, question, [
    { key: "cancel", label: pageSaveButton.dataset.cancel || "Cancel" },
    { key: "save", label: saveLabel },
  ]);
  return answer === "save";
}

/**
 * What a guest is told once their drawing is saved: that it is in this
 * browser and nowhere else, and what they can do about that. Closing it goes
 * back to the canvas; the drawing is kept either way.
 */
async function afterGuestSave(draft: LocalDraft, kept: boolean): Promise<void> {
  const png = new Blob([draft.png], { type: "image/png" });
  if (!kept) {
    const answer = await askInPainter(saveLabel, words.localSaveFailed || "This browser can't keep drawings.", [
      { key: "keep-drawing", label: words.keepDrawing || "Keep drawing" },
      { key: "download", label: words.downloadPng || "Download PNG" },
    ]);
    if (answer === "download") downloadPng(png, draft.savedAt);
    return;
  }

  markKept();
  for (;;) {
    const answer = await askInPainter(
      words.guestSavedTitle || saveLabel,
      words.guestSaved || "This drawing is only in this browser.",
      [
        { key: "download", label: words.downloadPng || "Download PNG" },
        { key: "sign-up", label: words.signUp || "Sign up" },
        { key: "sign-in", label: words.signIn || "Sign in" },
        { key: "done", label: words.done || "Done" },
      ],
    );
    if (answer === "download") {
      downloadPng(png, draft.savedAt);
      // Still worth saying where the drawing is, and offering the rest.
      continue;
    }
    const next = `?next=${encodeURIComponent(DRAFTS_PATH)}`;
    const destination =
      answer === "sign-up"
        ? `/signup${next}`
        : answer === "sign-in"
          ? `/login${next}`
          : answer === "done"
            ? DRAFTS_PATH
            : null;
    if (destination) {
      leaving = true;
      window.location.href = destination;
    }
    return;
  }
}

saveButton.addEventListener("click", () => {
  saveButton.disabled = true;
  void confirmSave().then(async (confirmed) => {
    if (!confirmed) {
      saveButton.disabled = false;
      return;
    }
    const submission = config.submission ?? { kind: "post" as const };
    if (submission.kind === "banner") {
      await submitBanner(painter, submission.profileUrl, startedAt, () => {
        leaving = true;
      }).catch((error) => {
        console.error(error);
        say("Failed to save drawing. Please try again.");
      });
      saveButton.disabled = false;
      return;
    }

    try {
      const { draft, kept, posted } = await submitPost(painter, config, startedAt, () => {
        leaving = true;
      });
      if (posted) return;
      if (!config.userId) {
        await afterGuestSave(draft, kept);
      } else if (kept) {
        // Signed in, but the upload failed: it waits in the drafts page.
        markKept();
        say(words.uploadFailedKept || "Couldn't post this drawing. It's kept in your drafts in this browser.");
      } else {
        say(words.uploadFailedLost || "Failed to save drawing. Please try again.");
      }
    } catch (error) {
      console.error(error);
      say("Failed to save drawing. Please try again.");
    }
    saveButton.disabled = false;
  });
});
