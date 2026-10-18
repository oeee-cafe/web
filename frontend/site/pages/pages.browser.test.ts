// The pages' own pieces of chrome.js, each run the way its template's line
// runs it, against just enough of its page.
import { afterEach, describe, expect, it, vi } from "vitest";
import { userEvent } from "vitest/browser";
import { installActions } from "../actions";
import { discordAccount } from "./discordAccount";
import { communityDraw, postPage, sessionDialog } from "./modals";
import { moveToCommunity, tagSuggestions } from "./postForm";
import { communitiesTabs, profileTabs } from "./tabs";
import { tegakiDraw, tegakiReplay } from "./tegaki";

type Page = Window & typeof globalThis & Record<string, unknown>;
const page = window as Page;

// The document's listeners for every declared control, as theme_head.jinja
// sets them up before any page.
installActions();

afterEach(() => {
  document.body.innerHTML = "";
  vi.unstubAllGlobals();
});

describe("Tegaki", () => {
  /** A stand-in Tegaki that records what is done to it, in order. */
  function standIn() {
    const done: string[] = [];
    const tegaki = {
      onTabClose: null as unknown,
      onCloseViewerClick: null as unknown,
      startTimeStamp: Date.now() - 5000,
      opened: null as Record<string, unknown> | null,
      open(options: Record<string, unknown>) {
        done.push("open" + (tegaki.onTabClose ? " after onTabClose" : ""));
        tegaki.opened = options;
      },
      updatePosOffset: () => done.push("updatePosOffset"),
      flatten: () => {
        const canvas = document.createElement("canvas");
        canvas.width = 40;
        canvas.height = 30;
        return canvas;
      },
      replayRecorder: { stop: () => done.push("stop"), toBlob: () => new Blob(["replay"]) },
      resume: () => done.push("resume"),
    };
    vi.stubGlobal("Tegaki", tegaki);
    vi.stubGlobal("TegakiHistory", { undoStack: [] as unknown[] });
    vi.stubGlobal("TegakiCursor", { updateCanvasSize: () => done.push("updateCanvasSize") });
    document.body.innerHTML = `<nav class="nav-bar" style="height: 40px"></nav><div id="tegaki"></div>`;
    return { tegaki, done };
  }

  it("asks before leaving only with something drawn, set before open() hooks it up", () => {
    const { tegaki, done } = standIn();
    tegakiDraw({ communityId: "c1", parentPostId: null, cancelTo: "/communities/@open", postError: "No", width: 400, height: 300 });
    expect(done[0]).toBe("open after onTabClose");
    expect(tegaki.opened).toMatchObject({ saveReplay: true, width: 400, height: 300 });
    // Under the toolbar, and measured again for the pen.
    expect(document.getElementById("tegaki")!.style.top).toBe("40px");
    expect(done).toContain("updateCanvasSize");

    const leave = (undo: unknown[]) => {
      (window as unknown as { TegakiHistory: { undoStack: unknown[] } }).TegakiHistory.undoStack = undo;
      const event = new Event("beforeunload", { cancelable: true }) as BeforeUnloadEvent;
      (tegaki.onTabClose as (event: BeforeUnloadEvent) => void)(event);
      return event.defaultPrevented;
    };
    expect(leave([])).toBe(false);
    expect(leave(["a stroke"])).toBe(true);
  });

  it("posts the drawing with what /draw/finish asks for", async () => {
    const { tegaki } = standIn();
    let sent: FormData | null = null;
    vi.stubGlobal("fetch", (_url: string, init: RequestInit) => {
      sent = init.body as FormData;
      return Promise.resolve(new Response(JSON.stringify({ post_id: "p1" }), { status: 500 }));
    });
    const alerted: string[] = [];
    vi.stubGlobal("alert", (text: string) => alerted.push(text));
    tegakiDraw({ communityId: "c1", parentPostId: "parent", cancelTo: "/", postError: "Could not post", width: 400, height: 300 });
    document.getElementById("tegaki")!.dispatchEvent(new Event("pointerdown"));
    (tegaki.opened!.onDone as () => void)();
    await expect.poll(() => alerted).toEqual(["Could not post"]);
    const form = sent as unknown as FormData;
    expect(form.get("tool")).toBe("tegaki");
    expect(form.get("community_id")).toBe("c1");
    expect(form.get("parent_post_id")).toBe("parent");
    expect(form.get("security_count")).toBe("1");
    expect(form.get("width")).toBe("40");
    expect(Number(form.get("paint_duration_ms"))).toBeGreaterThanOrEqual(5000);
  });

  it("plays a replay in replay mode, under the toolbar", () => {
    const { tegaki } = standIn();
    tegakiReplay("/@someone/p1", "https://r2.example/replay/ab/abc.tgkr");
    expect(tegaki.opened).toEqual({ replayMode: true, replayURL: "https://r2.example/replay/ab/abc.tgkr" });
    expect(document.getElementById("tegaki")!.style.top).toBe("40px");
  });
});

describe("the Windows app's Discord account", () => {
  it("shows what the app says and asks the app, not a page, to connect", async () => {
    const asked: string[] = [];
    let state: unknown = null;
    page.oeeeApp = { discord: { ask: (action: string) => asked.push(action), known: () => ({ state }) } };
    try {
      document.body.innerHTML = `
        <section id="account-discord-app" hidden>
          <span data-discord-status></span>
          <button data-discord-disconnect hidden>Disconnect</button>
          <button data-discord-connect hidden>Connect</button>
        </section>`;
      discordAccount({ off: "Not connected", connecting: "Connecting", connected: "Discord：{name}" });
      const section = document.getElementById("account-discord-app")!;
      expect(section.hidden).toBe(true);
      state = null;
      document.dispatchEvent(
        new CustomEvent("oeee:discord", { detail: { kind: "state", value: { connected: true, name: "a$&b" } } }),
      );
      expect(section.hidden).toBe(false);
      expect(section.querySelector("[data-discord-status]")!.textContent).toBe("Discord：a$&b");
      document.dispatchEvent(
        new CustomEvent("oeee:discord", { detail: { kind: "state", value: { connected: false } } }),
      );
      await userEvent.click(section.querySelector("[data-discord-connect]")!);
      expect(asked).toEqual(["connect"]);
    } finally {
      delete page.oeeeApp;
    }
  });
});

describe("the post page", () => {
  it("replies to a comment from its button, cancels, and empties the form once the comment is in", async () => {
    document.body.innerHTML = `
      <div id="comments"></div>
      <form id="comment-form">
        <input id="parent_comment_id" value="" />
        <div id="reply-info" style="display: none">
          <span id="reply-to-text"></span>
          <button type="button" id="cancel" data-action="cancel-reply">Cancel</button>
        </div>
        <textarea id="comment-content">Hello</textarea>
      </form>
      <button type="button" id="reply" data-comment-id="c9" data-replying-to="@neo" data-action="reply-to-comment">Reply</button>`;
    postPage();
    await userEvent.click(document.getElementById("reply")!);
    expect((document.getElementById("parent_comment_id") as HTMLInputElement).value).toBe("c9");
    expect(document.getElementById("reply-to-text")!.textContent).toBe("@neo");
    expect(document.activeElement).toBe(document.getElementById("comment-content"));
    await userEvent.click(document.getElementById("cancel")!);
    expect((document.getElementById("parent_comment_id") as HTMLInputElement).value).toBe("");
    expect(document.getElementById("reply-info")!.style.display).toBe("none");

    await userEvent.click(document.getElementById("reply")!);
    document.getElementById("comment-form")!.dispatchEvent(new Event("htmx:after:swap", { bubbles: true }));
    expect((document.getElementById("parent_comment_id") as HTMLInputElement).value).toBe("");
    expect(document.getElementById("reply-info")!.style.display).toBe("none");
  });

  it("opens its report with the description focused, and Escape closes it", async () => {
    document.body.innerHTML = `
      <button type="button" id="report" data-action="post-report-open">Report</button>
      <div id="report-modal" style="display: none"><h2>Report</h2><textarea id="report-description">old</textarea></div>`;
    postPage();
    await userEvent.click(document.getElementById("report")!);
    const modal = document.getElementById("report-modal")!;
    expect(modal.style.display).toBe("flex");
    expect((document.getElementById("report-description") as HTMLTextAreaElement).value).toBe("");
    expect(document.activeElement).toBe(document.getElementById("report-description"));
    await userEvent.keyboard("{Escape}");
    expect(modal.style.display).toBe("none");
  });

  it("sizes a New drawing by its orientation as it is sent, and opens and closes from its buttons", async () => {
    document.body.innerHTML = `
      <button type="button" id="open" data-action="community-draw-open">New drawing</button>
      <div id="community-draw-modal" style="display: none">
        <h2 id="community-draw-title" tabindex="-1"></h2>
        <form id="draw" data-orientation-size>
          <input type="radio" name="orientation" value="portrait" checked />
          <input type="radio" name="orientation" value="landscape" />
          <input name="width" /><input name="height" />
          <button type="button" id="close" data-action="community-draw-close">Cancel</button>
        </form>
      </div>`;
    communityDraw();
    const form = document.getElementById("draw") as HTMLFormElement;
    // Sent, as far as the page's listeners are concerned, and no further.
    form.addEventListener("submit", (event) => event.preventDefault());
    form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    const value = (name: string) => (form.elements.namedItem(name) as HTMLInputElement).value;
    expect([value("width"), value("height")]).toEqual(["480", "640"]);
    await userEvent.click(document.getElementById("open")!);
    expect(document.getElementById("community-draw-modal")!.style.display).toBe("flex");
    expect(document.activeElement).toBe(document.getElementById("community-draw-title"));
    await userEvent.click(document.getElementById("close")!);
    expect(document.getElementById("community-draw-modal")!.style.display).toBe("none");
  });

  it("opens New session from a button and closes it on a press outside", async () => {
    document.body.innerHTML = `
      <button type="button" id="open" data-action="session-dialog-open">New session</button>
      <div id="create-session-dialog" style="display: none"><h2 id="create-session-title" tabindex="-1"></h2></div>`;
    sessionDialog();
    await userEvent.click(document.getElementById("open")!);
    const dialog = document.getElementById("create-session-dialog")!;
    expect(dialog.style.display).toBe("flex");
    expect(document.activeElement).toBe(document.getElementById("create-session-title"));
    dialog.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(dialog.style.display).toBe("none");
  });

  it("leaves no globals for the markup to call", () => {
    for (const name of ["modalToggle", "replyToComment", "sharePost", "showReportModal", "toggleSessionDialog", "showCommunityDrawModal", "showProfileReportModal"]) {
      expect(name in window, name).toBe(false);
    }
  });
});

describe("the pages' other declared controls", () => {
  it("puts a notice away, and the list with its last one", async () => {
    document.body.innerHTML = `
      <div id="toasts">
        <div class="ds-notice" id="a">A<button type="button" data-action="dismiss-notice">x</button></div>
        <div class="ds-notice" id="b">B<button type="button" data-action="dismiss-notice">x</button></div>
      </div>`;
    await userEvent.click(document.querySelector("#a button")!);
    expect(document.getElementById("a")).toBeNull();
    expect(document.getElementById("toasts")!.innerHTML).not.toBe("");
    await userEvent.click(document.querySelector("#b button")!);
    expect(document.getElementById("toasts")!.innerHTML).toBe("");
  });

  it("closes a report's outcome, swapped in after the page was set up", async () => {
    document.body.innerHTML = `<div id="profile-report-modal" style="display: flex"><div id="slot"></div></div>`;
    document.getElementById("slot")!.innerHTML = `<button type="button" data-action="close-report">Close</button>`;
    await userEvent.click(document.querySelector("#slot button")!);
    expect(document.getElementById("profile-report-modal")!.style.display).toBe("none");
  });

  it("enables deleting a community once its slug is typed, and previews a new one's address", async () => {
    document.body.innerHTML = `
      <input id="confirm" data-slug="club" data-confirms-slug="delete" /><button id="delete" disabled>Delete</button>
      <input id="slug" data-slug-preview="where" /><span id="where"></span>`;
    await userEvent.type(document.getElementById("confirm")!, "clu");
    expect((document.getElementById("delete") as HTMLButtonElement).disabled).toBe(true);
    await userEvent.type(document.getElementById("confirm")!, "b");
    expect((document.getElementById("delete") as HTMLButtonElement).disabled).toBe(false);
    await userEvent.type(document.getElementById("slug")!, "art");
    expect(document.getElementById("where")!.textContent).toBe("/@art");
  });

  it("sends the language form on a choice, and keeps a form for show from being sent", () => {
    document.body.innerHTML = `
      <form id="language"><select id="pick" data-submit-on-change><option>a</option><option>b</option></select></form>
      <form id="show" data-no-submit><button>Send</button></form>`;
    let sent = 0;
    (document.getElementById("language") as HTMLFormElement).submit = () => {
      sent++;
    };
    const pick = document.getElementById("pick") as HTMLSelectElement;
    pick.value = "b";
    pick.dispatchEvent(new Event("change", { bubbles: true }));
    expect(sent).toBe(1);
    const send = new Event("submit", { bubbles: true, cancelable: true });
    document.getElementById("show")!.dispatchEvent(send);
    expect(send.defaultPrevented).toBe(true);
  });

  it("empties the guestbook's form once its entry is taken, and says a request that never arrived", () => {
    document.body.innerHTML = `
      <form id="guestbook" data-reset-after-request><textarea name="content">Hi</textarea></form>
      <form id="session" data-network-error="No connection"></form>`;
    const alerted: string[] = [];
    vi.stubGlobal("dsAlert", (text: string) => alerted.push(text));
    const guestbook = document.getElementById("guestbook") as HTMLFormElement;
    const textarea = guestbook.querySelector("textarea")!;
    textarea.value = "Hello";
    const after = (status: number) =>
      guestbook.dispatchEvent(new CustomEvent("htmx:after:request", { bubbles: true, detail: { ctx: { response: { status } } } }));
    after(422);
    expect(textarea.value).toBe("Hello");
    after(200);
    expect(textarea.value).toBe("Hi");

    const session = document.getElementById("session")!;
    session.dispatchEvent(new CustomEvent("htmx:error", { bubbles: true, detail: { ctx: { response: { status: 500 } } } }));
    expect(alerted).toEqual([]);
    session.dispatchEvent(new CustomEvent("htmx:error", { bubbles: true, detail: { ctx: {} } }));
    expect(alerted).toEqual(["No connection"]);
  });
});

describe("tabs", () => {
  it("brings the community directory back on the list chosen last", async () => {
    const markup = `
      <button data-communities-tab="public">Public</button>
      <button data-communities-tab="mine">Mine</button>
      <div data-communities-panel="public">P</div>
      <div data-communities-panel="mine" hidden>M</div>`;
    localStorage.removeItem("communitiesTab");
    document.body.innerHTML = markup;
    communitiesTabs();
    await userEvent.click(document.querySelector('[data-communities-tab="mine"]')!);
    expect(localStorage.getItem("communitiesTab")).toBe("mine");
    document.body.innerHTML = markup;
    communitiesTabs();
    expect(document.querySelector<HTMLElement>('[data-communities-panel="public"]')!.hidden).toBe(true);
    expect(document.querySelector('[data-communities-tab="mine"]')!.getAttribute("aria-pressed")).toBe("true");
    localStorage.removeItem("communitiesTab");
  });

  it("switches a profile's tab in place, each its own address", async () => {
    const start = location.href;
    document.body.innerHTML = `
      <a data-profile-tab="drawings" href="${location.pathname}" aria-current="page">Drawings</a>
      <a data-profile-tab="comments" href="${location.pathname}?tab=comments">Comments</a>
      <div data-profile-panel="drawings">D</div>
      <div data-profile-panel="comments" hidden>C</div>
      <div data-profile-per-row>Per row</div>`;
    profileTabs();
    await userEvent.click(document.querySelector('[data-profile-tab="comments"]')!);
    expect(location.search).toBe("?tab=comments");
    expect(document.querySelector<HTMLElement>('[data-profile-panel="drawings"]')!.hidden).toBe(true);
    expect(document.querySelector<HTMLElement>("[data-profile-per-row]")!.hidden).toBe(true);
    history.replaceState(null, "", start);
  });
});

describe("the publish page and Move to community", () => {
  it("moves through tag suggestions with the keys, and leaves Enter alone with none chosen", async () => {
    document.body.innerHTML = `
      <input id="tags-input" value="cat, do" />
      <div id="tag-suggestions">
        <div class="tag-autocomplete-item" id="t1" data-tag="dog">dog</div>
        <div class="tag-autocomplete-item" id="t2" data-tag="dolphin">dolphin</div>
      </div>`;
    tagSuggestions();
    document.dispatchEvent(new Event("DOMContentLoaded"));
    const input = document.getElementById("tags-input") as HTMLInputElement;
    input.focus();
    await userEvent.keyboard("{ArrowDown}{ArrowDown}");
    expect(input.getAttribute("aria-activedescendant")).toBe("t2");
    await userEvent.keyboard("{ArrowDown}");
    expect(input.getAttribute("aria-activedescendant")).toBe("t1");
    await userEvent.keyboard("{Enter}");
    expect(input.value).toBe("cat, dog, ");
    expect(document.getElementById("tag-suggestions")!.children.length).toBe(0);
  });

  it("starts a new batch of suggestions with nothing highlighted", async () => {
    document.body.innerHTML = `
      <input id="tags-input" value="do" aria-expanded="false" />
      <div id="tag-suggestions">
        <div class="tag-autocomplete-item" id="t1" data-tag="dog">dog</div>
        <div class="tag-autocomplete-item" id="t2" data-tag="dolphin">dolphin</div>
      </div>`;
    tagSuggestions();
    document.dispatchEvent(new Event("DOMContentLoaded"));
    const input = document.getElementById("tags-input") as HTMLInputElement;
    const menu = document.getElementById("tag-suggestions")!;
    input.focus();
    await userEvent.keyboard("{ArrowDown}");
    expect(input.getAttribute("aria-activedescendant")).toBe("t1");

    // As htmx 4 says it: on the input that asked, bubbling, once the menu
    // has the new batch.
    const swap = (html: string) => {
      menu.innerHTML = html;
      input.dispatchEvent(new CustomEvent("htmx:after:swap", { bubbles: true, detail: { ctx: { target: menu } } }));
    };
    swap(`<div class="tag-autocomplete-item" id="t3" data-tag="door">door</div>`);
    expect(input.getAttribute("aria-activedescendant")).toBe("");
    expect(input.getAttribute("aria-expanded")).toBe("true");
    // The first press goes to the first of the new batch, not past it.
    await userEvent.keyboard("{ArrowDown}");
    expect(input.getAttribute("aria-activedescendant")).toBe("t3");

    swap("");
    expect(input.getAttribute("aria-expanded")).toBe("false");
  });

  it("names the community chosen on the button, and Enter in the search picks the first", async () => {
    document.body.innerHTML = `
      <form id="move-form">
        <input id="community-search" />
        <label class="move-row"><input type="radio" name="community_id" value="a" data-move-label="Move to A" /></label>
        <label class="move-row"><input type="radio" name="community_id" value="b" data-move-label="Move to B" /></label>
        <button id="move-submit" disabled data-choose="Choose">Choose</button>
      </form>`;
    moveToCommunity();
    const submit = document.getElementById("move-submit") as HTMLButtonElement;
    await userEvent.click(document.querySelector('input[value="b"]')!);
    expect(submit.disabled).toBe(false);
    expect(submit.textContent).toBe("Move to B");
    document.getElementById("community-search")!.focus();
    await userEvent.keyboard("{Enter}");
    expect(submit.textContent).toBe("Move to A");
    // A search that leaves the chosen row out takes the choice off the button.
    document.querySelectorAll(".move-row")[0].remove();
    document.querySelectorAll(".move-row")[0].remove();
    document.dispatchEvent(new Event("htmx:after:swap"));
    expect(submit.disabled).toBe(true);
    expect(submit.textContent).toBe("Choose");
  });
});
