// The pages' own modals: the post page's (post_view.jinja), a community's New
// drawing (community.jinja), New session (collaborate_create_dialog.jinja)
// and the profile's report (profile.jinja). Their buttons open and close them
// through data-action (actions.ts); each page's piece wires what is the
// page's own -- a press on the dimmed page, Escape.

type ModalWindow = Window &
  typeof globalThis & {
    oeeeApp?: { connected(): boolean; feel(name: string): void };
    dsAlert?: (text: string) => void;
  };

const page = window as ModalWindow;

function byId<T extends HTMLElement = HTMLElement>(id: string): T | null {
  return document.getElementById(id) as T | null;
}

/** Portrait or landscape, as the canvas size a New drawing asks for. */
export function setOrientationDimensions(form: HTMLFormElement): void {
  const portrait = form.querySelector<HTMLInputElement>('input[name="orientation"]:checked')!.value === "portrait";
  form.querySelector<HTMLInputElement>('input[name="width"]')!.value = portrait ? "480" : "640";
  form.querySelector<HTMLInputElement>('input[name="height"]')!.value = portrait ? "640" : "480";
}

export function modalToggle(id: string, open: boolean): void {
  const modal = byId(id);
  if (!modal) return;
  modal.style.display = open ? "flex" : "none";
  if (open) {
    // The report's description is there to be typed into, so it takes focus.
    // Anything else focuses the heading: on iOS a focused <select> opens its
    // picker, so the width wheel would come up with the modal.
    const first = modal.querySelector<HTMLElement>("textarea") || modal.querySelector<HTMLElement>("h2");
    if (first) first.focus();
  }
}

/** A comment's Reply (comments_macro.jinja): the form answers that comment, says whom, and comes into view. */
export function replyToComment(button: Element): void {
  const form = byId("comment-form");
  if (!form) return;
  byId<HTMLInputElement>("parent_comment_id")!.value = button.getAttribute("data-comment-id")!;
  byId("reply-to-text")!.textContent = button.getAttribute("data-replying-to");
  byId("reply-info")!.style.display = "block";
  byId("comment-content")!.focus();
  form.scrollIntoView({ behavior: "smooth", block: "center" });
}

/** The reply's own Cancel: the form answers the post again. */
export function cancelReply(): void {
  byId<HTMLInputElement>("parent_comment_id")!.value = "";
  byId("reply-info")!.style.display = "none";
}

export function showReportModal(): void {
  const text = byId<HTMLTextAreaElement>("report-description");
  if (text) text.value = "";
  modalToggle("report-modal", true);
}

export function sharePost(button: Element): void {
  const url = button.getAttribute("data-share-url")!;
  if (navigator.share) {
    navigator.share({ title: button.getAttribute("data-share-title")!, url }).catch(() => {});
  } else if (navigator.clipboard) {
    navigator.clipboard.writeText(url).then(() => {
      if (page.oeeeApp && page.oeeeApp.connected()) page.oeeeApp.feel("success");
      page.dsAlert!(button.getAttribute("data-share-copied")!);
    });
  }
}

export function showCommunityDrawModal(): void {
  const modal = byId("community-draw-modal")!;
  modal.style.display = "flex";
  // Focus the heading, not the first field: on iOS a focused <select> opens
  // its picker, so the width wheel would come up with the modal.
  byId("community-draw-title")!.focus();
}

export function hideCommunityDrawModal(): void {
  byId("community-draw-modal")!.style.display = "none";
}

export function toggleSessionDialog(open: boolean): void {
  const dialog = byId("create-session-dialog")!;
  dialog.style.display = open ? "flex" : "none";
  // Focus the heading, not the first field: on iOS a focused <select> opens
  // its picker and a focused input raises the keyboard, so New session would
  // arrive with the canvas-size wheel already up.
  if (open) byId("create-session-title")!.focus();
}

export function showProfileReportModal(): void {
  const modal = byId("profile-report-modal")!;
  modal.style.display = "flex";
  byId<HTMLTextAreaElement>("profile-report-description")!.value = "";
  byId("profile-report-description")!.focus();
}

export function hideProfileReportModal(): void {
  byId("profile-report-modal")!.style.display = "none";
}

/** The post page: its two modals close on a press outside and on Escape, and the comment form empties once its comment is in. */
export function postPage(): void {
  const modals = ["post-draw-modal", "report-modal"];
  for (let i = 0; i < modals.length; i++) {
    const modal = byId(modals[i]);
    if (!modal) continue;
    modal.addEventListener("click", function (this: HTMLElement, e) {
      if (e.target === this) this.style.display = "none";
    });
  }
  document.addEventListener("keydown", (e) => {
    if (e.key !== "Escape") return;
    for (let j = 0; j < modals.length; j++) modalToggle(modals[j], false);
  });
  // Once the list has taken the new comment, the form is emptied and no
  // longer replying to anyone. htmx 4 says so on the form that asked; htmx 1
  // said it on the list.
  function afterSwap(e: Event): void {
    const target = e.target as Element | null;
    if (!target || (target.id !== "comments" && target.id !== "comment-form")) return;
    const form = byId<HTMLFormElement>("comment-form");
    if (!form) return;
    form.reset();
    cancelReply();
  }
  document.addEventListener("htmx:after:swap", afterSwap);
  document.addEventListener("htmx:afterSwap", afterSwap);
}

/** A community's New drawing closes on a press outside and on Escape. */
export function communityDraw(): void {
  const modal = byId("community-draw-modal")!;
  modal.addEventListener("click", (e) => {
    if (e.target === modal) hideCommunityDrawModal();
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && modal.style.display !== "none") hideCommunityDrawModal();
  });
}

/** New session closes on a press outside and on Escape. */
export function sessionDialog(): void {
  const dialog = byId("create-session-dialog")!;
  dialog.addEventListener("click", (e) => {
    if (e.target === dialog) toggleSessionDialog(false);
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && dialog.style.display !== "none") toggleSessionDialog(false);
  });
}

/** The profile's report closes on a press outside. */
export function profileReport(): void {
  byId("profile-report-modal")!.addEventListener("click", function (this: HTMLElement, e) {
    if (e.target === this) hideProfileReportModal();
  });
}
