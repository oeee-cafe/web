/**
 * The drafts page's drawings that only this browser has.
 *
 * A guest's are posted once they have an account; someone signed in sees
 * their own whose upload never went through. Posting one is the upload the
 * painter would have made, after which it is a draft like any other and goes
 * to its publish page. The server's drafts are the page's own markup
 * (templates/draft_posts.jinja); this adds a section above them.
 */
import {
  deleteLocalDraft,
  draftPng,
  listLocalDrafts,
  localDraftsAvailable,
  type LocalDraft,
} from "../shared/localDrafts";
import { feelInApp } from "../shared/appBridge";
import { downloadPng, UploadError, uploadLocalDraft } from "../shared/drawingUpload";
import { ask, say } from "../shared/siteDialog";

interface DraftsWords {
  untitled: string;
  personal: string;
  post: string;
  signIn: string;
  download: string;
  delete: string;
  deleteConfirm: string;
  communityDenied: string;
  postFailed: string;
  unavailable: string;
}

const section = document.getElementById("local-drafts");
const grid = document.getElementById("local-drafts-grid");
const guestEmpty = document.getElementById("local-drafts-empty");
const words = JSON.parse(
  document.getElementById("local-drafts-words")?.textContent || "{}",
) as Partial<DraftsWords>;
const userId = section?.dataset.userId || null;
const DRAFTS_PATH = "/posts/drafts";

function button(label: string, primary = false): HTMLButtonElement {
  const element = document.createElement("button");
  element.type = "button";
  element.className = `ds-button ds-button-small${primary ? " ds-button-primary" : ""}`;
  element.textContent = label;
  return element;
}

function signInLink(): HTMLAnchorElement {
  const link = document.createElement("a");
  link.className = "ds-button ds-button-primary ds-button-small";
  link.href = `/login?next=${encodeURIComponent(DRAFTS_PATH)}`;
  link.textContent = words.signIn || "Sign in";
  return link;
}

function removeCard(card: HTMLElement): void {
  card.remove();
  if (grid && !grid.children.length && section) {
    section.hidden = true;
    if (guestEmpty) guestEmpty.hidden = false;
  }
}

async function post(draft: LocalDraft, card: HTMLElement, trigger: HTMLButtonElement): Promise<void> {
  trigger.disabled = true;
  let withoutCommunity = false;
  for (;;) {
    try {
      const result = await uploadLocalDraft(draft, { withoutCommunity });
      await deleteLocalDraft(draft.id).catch(console.error);
      removeCard(card);
      // A fetch, so the page's listeners (theme_head.jinja) never hear how it went.
      feelInApp("success");
      window.location.href = `/posts/${result.post_id}/publish`;
      return;
    } catch (error) {
      console.error(error);
      const code = error instanceof UploadError ? error.code : null;
      if (code === "COMMUNITY_NOT_ALLOWED" && !withoutCommunity) {
        if (await ask(words.communityDenied || "Post it as a personal drawing instead?", words.post, "plain")) {
          withoutCommunity = true;
          continue;
        }
      } else if (code === "UNAUTHORIZED") {
        // Signed out since this page loaded.
        window.location.href = `/login?next=${encodeURIComponent(DRAFTS_PATH)}`;
        return;
      } else {
        feelInApp("error");
        say(words.postFailed || "Couldn't post this drawing.");
      }
      trigger.disabled = false;
      return;
    }
  }
}

function card(draft: LocalDraft): HTMLElement {
  const item = document.createElement("div");
  item.className = "posts-grid-item draft-item local-draft-item";

  const png = draftPng(draft);
  const pngUrl = URL.createObjectURL(png);
  // The grid draws a drawing absolutely inside a square frame, and the frame
  // is the item's first link (style.css, .posts-grid-item > a:first-child);
  // an image without one fills the page. Opening it shows it full size.
  const frame = document.createElement("a");
  frame.href = pngUrl;
  frame.target = "_blank";
  frame.rel = "noopener";
  const image = document.createElement("img");
  image.width = draft.width;
  image.height = draft.height;
  image.alt = words.untitled || "Untitled";
  image.decoding = "async";
  image.src = pngUrl;
  frame.append(image);
  if (draft.width > 300 && draft.height > 300) image.className = "drawing-downscaled";

  const meta = document.createElement("div");
  meta.className = "post-card-meta";

  const title = document.createElement("div");
  title.className = "post-card-title is-untitled";
  title.textContent = words.untitled || "Untitled";

  const byline = document.createElement("div");
  byline.className = "post-card-byline";
  const who = document.createElement("span");
  who.className = "post-card-who";
  who.textContent = draft.communityName || words.personal || "Personal post";
  const when = document.createElement("span");
  when.className = "post-card-when";
  const saved = new Date(draft.savedAt);
  when.textContent = saved.toLocaleDateString(document.documentElement.lang || undefined);
  when.title = saved.toLocaleString(document.documentElement.lang || undefined);
  byline.append(who, when);

  const actions = document.createElement("div");
  actions.className = "draft-actions";
  if (userId) {
    const postButton = button(words.post || "Post", true);
    postButton.addEventListener("click", () => void post(draft, item, postButton));
    actions.append(postButton);
  } else {
    actions.append(signInLink());
  }
  const downloadButton = button(words.download || "Download PNG");
  downloadButton.addEventListener("click", () => downloadPng(png, draft.savedAt));
  const deleteButton = button(words.delete || "Delete");
  deleteButton.addEventListener("click", () => {
    void ask(words.deleteConfirm || "Delete this drawing from this device?", words.delete).then(
      async (confirmed) => {
        if (!confirmed) return;
        await deleteLocalDraft(draft.id);
        removeCard(item);
      },
    ).catch(console.error);
  });
  actions.append(downloadButton, deleteButton);

  meta.append(title, byline, actions);
  item.append(frame, meta);
  return item;
}

async function show(): Promise<void> {
  if (!section || !grid) return;
  if (!(await localDraftsAvailable())) {
    // Only worth saying to a guest: signed in, the server's drafts are all
    // there is, and nothing is missing from them.
    if (!userId && guestEmpty) {
      const note = document.createElement("p");
      note.className = "profile-empty";
      note.textContent = words.unavailable || "";
      guestEmpty.append(note);
    }
    return;
  }
  const drafts = await listLocalDrafts(userId);
  if (!drafts.length) return;
  grid.append(...drafts.map(card));
  section.hidden = false;
  if (guestEmpty) guestEmpty.hidden = true;
}

void show().catch(console.error);
