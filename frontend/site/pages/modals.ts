// The pages' own modals, opened and closed from their buttons' onclick
// attributes: the post page's (post_view.jinja), a community's New drawing
// (community.jinja), New session (collaborate_create_dialog.jinja) and the
// profile's report (profile.jinja). Each piece puts the functions those
// attributes name on the window, as the page's script declared them, every
// time it runs, and wires the modals the page has now.

type ModalWindow = Window &
  typeof globalThis & {
    oeeeApp?: { connected(): boolean; feel(name: string): void };
    dsAlert?: (text: string) => void;
    [name: string]: unknown;
  };

const page = window as ModalWindow;

function byId<T extends HTMLElement = HTMLElement>(id: string): T | null {
  return document.getElementById(id) as T | null;
}

// Portrait or landscape, as the canvas size a New drawing asks for.
function setOrientationDimensions(form: HTMLFormElement): void {
  const portrait = form.querySelector<HTMLInputElement>('input[name="orientation"]:checked')!.value === "portrait";
  form.querySelector<HTMLInputElement>('input[name="width"]')!.value = portrait ? "480" : "640";
  form.querySelector<HTMLInputElement>('input[name="height"]')!.value = portrait ? "640" : "480";
}

function modalToggle(id: string, open: boolean): void {
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

/** The post page: its two modals, Reply, Share, and the comment form after a comment. */
export function postPage(): void {
  page.modalToggle = modalToggle;
  // A comment's Reply (comments_macro.jinja): the form answers that comment,
  // says whom, and comes into view.
  page.replyToComment = (button: Element) => {
    const form = byId("comment-form");
    if (!form) return;
    byId<HTMLInputElement>("parent_comment_id")!.value = button.getAttribute("data-comment-id")!;
    byId("reply-to-text")!.textContent = button.getAttribute("data-replying-to");
    byId("reply-info")!.style.display = "block";
    byId("comment-content")!.focus();
    form.scrollIntoView({ behavior: "smooth", block: "center" });
  };
  page.showPostDrawModal = () => modalToggle("post-draw-modal", true);
  page.hidePostDrawModal = () => modalToggle("post-draw-modal", false);
  page.showReportModal = () => {
    const text = byId<HTMLTextAreaElement>("report-description");
    if (text) text.value = "";
    modalToggle("report-modal", true);
  };
  page.hideReportModal = () => modalToggle("report-modal", false);
  page.setOrientationDimensions = setOrientationDimensions;
  page.sharePost = (button: Element) => {
    const url = button.getAttribute("data-share-url")!;
    if (navigator.share) {
      navigator.share({ title: button.getAttribute("data-share-title")!, url }).catch(() => {});
    } else if (navigator.clipboard) {
      navigator.clipboard.writeText(url).then(() => {
        if (page.oeeeApp && page.oeeeApp.connected()) page.oeeeApp.feel("success");
        page.dsAlert!(button.getAttribute("data-share-copied")!);
      });
    }
  };

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
    byId<HTMLInputElement>("parent_comment_id")!.value = "";
    byId("reply-info")!.style.display = "none";
  }
  document.addEventListener("htmx:after:swap", afterSwap);
  document.addEventListener("htmx:afterSwap", afterSwap);
}

/** A community's New drawing modal. */
export function communityDraw(): void {
  page.showCommunityDrawModal = () => {
    const modal = byId("community-draw-modal")!;
    modal.style.display = "flex";
    // Focus the heading, not the first field: on iOS a focused <select> opens
    // its picker, so the width wheel would come up with the modal.
    byId("community-draw-title")!.focus();
  };
  const hide = () => {
    byId("community-draw-modal")!.style.display = "none";
  };
  page.hideCommunityDrawModal = hide;
  page.setOrientationDimensions = setOrientationDimensions;
  const modal = byId("community-draw-modal")!;
  modal.addEventListener("click", (e) => {
    if (e.target === modal) hide();
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && modal.style.display !== "none") hide();
  });
}

/** New session (collaborate_create_dialog.jinja), on the lobby and a community's page. */
export function sessionDialog(): void {
  const toggleSessionDialog = (open: boolean) => {
    const dialog = byId("create-session-dialog")!;
    dialog.style.display = open ? "flex" : "none";
    // Focus the heading, not the first field: on iOS a focused <select> opens
    // its picker and a focused input raises the keyboard, so New session
    // would arrive with the canvas-size wheel already up.
    if (open) byId("create-session-title")!.focus();
  };
  page.toggleSessionDialog = toggleSessionDialog;
  const dialog = byId("create-session-dialog")!;
  dialog.addEventListener("click", (e) => {
    if (e.target === dialog) toggleSessionDialog(false);
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && dialog.style.display !== "none") toggleSessionDialog(false);
  });
}

/** Reporting a profile. */
export function profileReport(): void {
  page.showProfileReportModal = () => {
    const modal = byId("profile-report-modal")!;
    modal.style.display = "flex";
    byId<HTMLTextAreaElement>("profile-report-description")!.value = "";
    byId("profile-report-description")!.focus();
  };
  const hide = () => {
    byId("profile-report-modal")!.style.display = "none";
  };
  page.hideProfileReportModal = hide;
  // Close modal when clicking outside
  byId("profile-report-modal")!.addEventListener("click", function (this: HTMLElement, e) {
    if (e.target === this) hide();
  });
}
