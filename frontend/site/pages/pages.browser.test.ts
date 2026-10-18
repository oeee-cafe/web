// The pages' own pieces of chrome.js, each run the way its template's line
// runs it, against just enough of its page.
import { afterEach, describe, expect, it, vi } from "vitest";
import { userEvent } from "vitest/browser";
import { discordAccount } from "./discordAccount";
import { communityDraw, postPage, sessionDialog } from "./modals";
import { moveToCommunity, tagSuggestions } from "./postForm";
import { communitiesTabs, profileTabs } from "./tabs";
import { tegakiDraw, tegakiReplay } from "./tegaki";

type Page = Window & typeof globalThis & Record<string, unknown>;
const page = window as Page;

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
  it("replies to a comment, and empties the form once the comment is in", () => {
    document.body.innerHTML = `
      <div id="comments"></div>
      <form id="comment-form">
        <input id="parent_comment_id" value="" />
        <div id="reply-info" style="display: none"><span id="reply-to-text"></span></div>
        <textarea id="comment-content">Hello</textarea>
      </form>
      <button id="reply" data-comment-id="c9" data-replying-to="@neo">Reply</button>`;
    postPage();
    (page.replyToComment as (button: Element) => void)(document.getElementById("reply")!);
    expect((document.getElementById("parent_comment_id") as HTMLInputElement).value).toBe("c9");
    expect(document.getElementById("reply-to-text")!.textContent).toBe("@neo");
    expect(document.activeElement).toBe(document.getElementById("comment-content"));
    document.getElementById("comment-form")!.dispatchEvent(new Event("htmx:after:swap", { bubbles: true }));
    expect((document.getElementById("parent_comment_id") as HTMLInputElement).value).toBe("");
    expect(document.getElementById("reply-info")!.style.display).toBe("none");
  });

  it("opens its report with the description focused, and Escape closes it", async () => {
    document.body.innerHTML = `
      <div id="report-modal" style="display: none"><h2>Report</h2><textarea id="report-description">old</textarea></div>`;
    postPage();
    (page.showReportModal as () => void)();
    const modal = document.getElementById("report-modal")!;
    expect(modal.style.display).toBe("flex");
    expect((document.getElementById("report-description") as HTMLTextAreaElement).value).toBe("");
    expect(document.activeElement).toBe(document.getElementById("report-description"));
    await userEvent.keyboard("{Escape}");
    expect(modal.style.display).toBe("none");
  });

  it("sizes a New drawing by its orientation", () => {
    document.body.innerHTML = `
      <form id="draw">
        <input type="radio" name="orientation" value="portrait" checked />
        <input type="radio" name="orientation" value="landscape" />
        <input name="width" /><input name="height" />
      </form>`;
    // A community's page defines it too, for its own New drawing.
    document.body.insertAdjacentHTML("beforeend", `<div id="community-draw-modal" style="display: none"><h2 id="community-draw-title" tabindex="-1"></h2></div>`);
    communityDraw();
    const form = document.getElementById("draw") as HTMLFormElement;
    (page.setOrientationDimensions as (form: HTMLFormElement) => void)(form);
    const value = (name: string) => (form.elements.namedItem(name) as HTMLInputElement).value;
    expect([value("width"), value("height")]).toEqual(["480", "640"]);
    (page.showCommunityDrawModal as () => void)();
    expect(document.getElementById("community-draw-modal")!.style.display).toBe("flex");
    (page.hideCommunityDrawModal as () => void)();
    expect(document.getElementById("community-draw-modal")!.style.display).toBe("none");
  });

  it("opens New session from a button and closes it on a press outside", () => {
    document.body.innerHTML = `
      <div id="create-session-dialog" style="display: none"><h2 id="create-session-title" tabindex="-1"></h2></div>`;
    sessionDialog();
    (page.toggleSessionDialog as (open: boolean) => void)(true);
    const dialog = document.getElementById("create-session-dialog")!;
    expect(dialog.style.display).toBe("flex");
    expect(document.activeElement).toBe(document.getElementById("create-session-title"));
    dialog.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(dialog.style.display).toBe("none");
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
