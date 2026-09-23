/**
 * What the site says to the apps, checked against the shape the apps read.
 *
 * The apps -- oeee-cafe-apple, oeee-cafe-android and oeee-cafe-desktop --
 * parse every message the page sends on oeeeBridge (app_bridge.jinja) and
 * drop one they cannot read without a word, so a field renamed or retyped
 * here would break them with nothing failing on this side. They each used to
 * keep a copy of captured messages to test against, copied by hand and never
 * recaptured; this is where that check lives now, against the templates as
 * they are.
 *
 * The head is rendered the way the server renders it, near enough: the
 * templates' own includes are followed, comments are dropped, and a message
 * from the catalogues is its id as a JSON string, which is what the Rust
 * tests' ftl_get_message stub answers too. Each case gets a fresh page in an
 * iframe, shaped like the site's where it matters -- the toolbar, the bell,
 * the presence meta, the design tokens -- with a stand-in oeeeBridge that
 * records what it is told.
 */
import { afterEach, describe, expect, it } from "vitest";

const sources = import.meta.glob("../../templates/*.jinja", {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;

function template(name: string): string {
  const source = sources[`../../templates/${name}`];
  if (source === undefined) throw new Error(`${name} is not in templates/`);
  return source
    .replace(/\{#[\s\S]*?#\}/g, "")
    .replace(/\{%\s*include\s+"([^"]+)"\s*%\}/g, (_, included: string) => template(included))
    .replace(/\{\{\s*ftl_get_message\("([^"]+)"\)\|tojson\s*\}\}/g, (_, id: string) =>
      JSON.stringify(id),
    );
}

const HEAD = template("theme_head.jinja");

type Message = { v: number; type: string } & Record<string, unknown>;

/** The page's window, with what the head defines on it. */
type PageWindow = Window &
  typeof globalThis & {
    oeeeApp: {
      report(): void;
      feel(name: string): void;
      wouldLoseWork(): boolean;
      pushToken(token: string): void;
      store: {
        purchased(proofs: unknown): Promise<string[]>;
        ticket(): Promise<{ ticket: string; user: string } | null>;
      };
      signIn: {
        answer(told: Record<string, unknown>): void;
        resume(): void;
        unopened(): void;
      };
    };
  };

interface Hooks {
  send(text: unknown): void;
  fetch(url: string, init?: RequestInit): Promise<Response>;
}

interface Page {
  frame: HTMLIFrameElement;
  window: PageWindow;
  sent: Message[];
  /** The requests the page made, and what each was answered with. */
  asked: { url: string; body: string }[];
}

interface Options {
  userAgent?: string;
  /** The store the build sells through, named in its user agent as the builds name it. */
  store?: "apple" | "microsoft" | "steam";
  signedIn?: boolean;
  presence?: string;
  unread?: number;
  /** What each path the page fetches answers with, as JSON; absent is a 404. */
  answers?: Record<string, unknown>;
}

const frames: HTMLIFrameElement[] = [];
afterEach(() => {
  for (const frame of frames.splice(0)) frame.remove();
});

async function open(options: Options = {}): Promise<Page> {
  const sent: Message[] = [];
  const asked: { url: string; body: string }[] = [];
  const hooks: Hooks = {
    send(text: unknown) {
      // Android's listener carries only strings, so every message is one.
      expect(typeof text).toBe("string");
      sent.push(JSON.parse(text as string));
    },
    fetch(url: string, init?: RequestInit) {
      asked.push({ url, body: String(init?.body ?? "") });
      const answer = options.answers?.[new URL(url, "https://oeee.cafe/").pathname];
      return Promise.resolve(
        new Response(answer === undefined ? "" : JSON.stringify(answer), {
          status: answer === undefined ? 404 : 200,
        }),
      );
    },
  };
  const before = `<script>
    window.oeeeBridge = { postMessage: function (text) { parent.__appContract.send(text); } };
    // The site is asked through the test; a blob or data URL the page reads
    // for itself is the browser's.
    var ownFetch = window.fetch;
    window.fetch = function (url, init) {
      return /^(blob|data):/.test(String(url)) ? ownFetch(url, init) : parent.__appContract.fetch(String(url), init);
    };
    Object.defineProperty(navigator, "userAgent", { get: function () { return ${JSON.stringify(
      (options.userAgent ?? "Mozilla/5.0 OeeeCafe/android") + (options.store ? ` store/${options.store}` : ""),
    )}; } });
    // Desktop Chromium has Web Share, which Android's web view does not.
    delete Navigator.prototype.share;
    delete Navigator.prototype.canShare;
  </script>`;
  const presence = options.presence
    ? `<meta name="oeee-presence" content="${options.presence}" data-community="오이카페 &quot;모에화&quot;" data-group="0123456789abcdef">`
    : "";
  const body = `
    <nav class="nav-bar" data-window-drag${options.signedIn ? " data-signed-in" : ""}
         style="background-color: rgb(250, 250, 252)">
      <a id="nav-notifications" data-unread="${options.unread ?? 0}"></a>
    </nav>
    <a href="/@artist/9c881320"><img data-oeee-drawing width="300" height="200"
       src="data:image/gif;base64,R0lGODlhAQABAAAAACw="></a>
    <a class="auth-apple" href="/auth/apple?next=%2Fafter">Apple</a>
    <a class="auth-google" href="/auth/google?next=%2Fafter">Google</a>
    <a class="auth-steam" href="/auth/steam/app?next=%2Fafter">Steam</a>`;
  const html = `<!doctype html><html><head>${before}
    <style>:root { --ds-ground: #ccccff; --ds-grid: #bbbbff; } body { background: rgb(255, 255, 255); }</style>
    ${presence}${HEAD}</head><body>${body}</body></html>`;

  (window as unknown as { __appContract: Hooks }).__appContract = hooks;
  const frame = document.createElement("iframe");
  frames.push(frame);
  const loaded = new Promise((resolve) => frame.addEventListener("load", resolve, { once: true }));
  frame.srcdoc = html;
  document.body.appendChild(frame);
  await loaded;
  return { frame, window: frame.contentWindow as PageWindow, sent, asked };
}

function last(page: Page, type: string): Message {
  const found = page.sent.filter((message) => message.type === type).pop();
  if (!found) throw new Error(`no ${type} message; sent ${page.sent.map((m) => m.type)}`);
  return found;
}

/** Exactly these keys, so a field added or renamed shows up here first. */
function keys(message: Message): string[] {
  return Object.keys(message).sort();
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("what the site tells the apps", () => {
  it("says who is signed in, what they are doing, and what they may do", async () => {
    const page = await open({ signedIn: true, presence: "collaborating" });
    const message = last(page, "page");
    expect(keys(message)).toEqual(
      ["community", "group", "painting", "path", "presence", "signedIn", "type", "v"],
    );
    expect(message).toMatchObject({
      v: 1,
      signedIn: true,
      presence: "collaborating",
      community: '오이카페 "모에화"',
      group: "0123456789abcdef",
      painting: true,
    });
    expect(typeof message.path).toBe("string");
  });

  it("cannot say who is signed in on a page without the toolbar, and says so", async () => {
    const page = await open();
    page.window.document.querySelector(".nav-bar")!.remove();
    page.window.oeeeApp.report();
    expect(last(page, "page")).toMatchObject({ signedIn: null, presence: null, painting: false });
  });

  it("gives the unread count as a number", async () => {
    const page = await open({ unread: 12 });
    expect(last(page, "unread")).toEqual({ v: 1, type: "unread", count: 12 });
  });

  it("gives the theme's choice and the design system's ground", async () => {
    const page = await open();
    const message = last(page, "theme");
    expect(keys(message)).toEqual(["choice", "grid", "ground", "type", "v"]);
    expect(message).toMatchObject({
      choice: "system",
      ground: "#ccccff",
      grid: "#bbbbff",
    });
  });

  it("gives every word an app says over the page, as strings", async () => {
    const page = await open();
    const message = last(page, "words");
    expect(keys(message)).toEqual(
      [
        "cancel", "copyImage", "copyLink", "leave", "leaveBody", "leaveTitle", "ok", "saveFailed",
        "saveImage", "savedFile", "savedImage", "share", "stay", "type", "v",
      ],
    );
    for (const [key, value] of Object.entries(message)) {
      if (key !== "v") expect(typeof value, key).toBe("string");
    }
  });

  it("says which drawing a finger lands on, as it lands", async () => {
    const page = await open();
    const image = page.window.document.querySelector("img")!;
    image.dispatchEvent(new page.window.Event("touchstart", { bubbles: true }));
    const message = last(page, "pressed");
    expect(Object.keys(message.drawing as object).sort()).toEqual(["height", "link", "src", "width"]);
    const drawing = message.drawing as Record<string, unknown>;
    expect(drawing.link).toMatch(/\/@artist\/9c881320$/);
    expect(drawing).toMatchObject({ width: 300, height: 200 });
  });

  it("sends a haptic on the bridge", async () => {
    const page = await open();
    page.window.oeeeApp.feel("success");
    expect(last(page, "haptic")).toEqual({ v: 1, type: "haptic", name: "success" });
  });

  it("says whether leaving would lose work, from the page's own beforeunload", async () => {
    const page = await open();
    expect(page.window.oeeeApp.wouldLoseWork()).toBe(false);
    page.window.addEventListener("beforeunload", (event: Event) => event.preventDefault());
    expect(page.window.oeeeApp.wouldLoseWork()).toBe(true);
  });

  it("marks the root with which app, what kind of device, and where it sells, from the user agent", async () => {
    const marks = (root: HTMLElement) =>
      ["data-app", "data-form", "data-store"].map((name) => root.getAttribute(name));
    const cases: [string, (string | null)[]][] = [
      ["Mozilla/5.0 OeeeCafe/ios store/apple", ["ios", "handheld", "apple"]],
      ["Mozilla/5.0 OeeeCafe/android", ["android", "handheld", null]],
      ["Mozilla/5.0 OeeeCafe/macos store/apple", ["macos", "desktop", "apple"]],
      ["Mozilla/5.0 Edg/120 OeeeCafe/windows store/steam", ["windows", "desktop", "steam"]],
      ["Mozilla/5.0 Edg/120 OeeeCafe/windows store/microsoft", ["windows", "desktop", "microsoft"]],
      ["Mozilla/5.0 Edg/120 OeeeCafe/windows", ["windows", "desktop", null]],
      // A browser, and a store named with no app to be in.
      ["Mozilla/5.0 Firefox/56.0", [null, null, null]],
      ["Mozilla/5.0 store/apple", [null, null, null]],
      // Only the names these are, and only as words of their own. The
      // server reads the same cases the same way (Store::from_user_agent).
      ["Mozilla/5.0 OeeeCafe/tv store/nowhere", [null, null, null]],
      ["Mozilla/5.0 OeeeCafe/iosx store/apple", [null, null, null]],
      ["Mozilla/5.0 OeeeCafe/ios store/applesauce", ["ios", "handheld", null]],
      ["Mozilla/5.0 OeeeCafe/ios mystore/apple", ["ios", "handheld", null]],
      ["Mozilla/5.0 XOeeeCafe/ios store/apple", [null, null, null]],
    ];
    for (const [userAgent, expected] of cases) {
      const page = await open({ userAgent });
      expect(marks(page.window.document.documentElement), userAgent).toEqual(expected);
    }
  });

  it("sends haptics to any app listening, whatever it is on", async () => {
    const page = await open({ userAgent: "Mozilla/5.0 OeeeCafe/macos" });
    const button = page.window.document.createElement("button");
    button.setAttribute("data-haptic", "light");
    page.window.document.body.appendChild(button);
    button.click();
    expect(last(page, "haptic")).toEqual({ v: 1, type: "haptic", name: "light" });
  });
});

describe("push notifications for an app", () => {
  it("registers the app's token for whoever is signed in, once, with the platform its user agent names", async () => {
    const page = await open({ signedIn: true, answers: { "/api/v1/devices": { id: "D" } } });
    page.window.oeeeApp.pushToken("T1");
    page.window.oeeeApp.pushToken("T1");
    await settle();
    const posted = page.asked.filter((request) => request.url === "/api/v1/devices");
    expect(posted.map((request) => JSON.parse(request.body))).toEqual([{ device_token: "T1", platform: "android" }]);

    // A token the app was given since is registered too.
    page.window.oeeeApp.pushToken("T2");
    await settle();
    expect(page.asked.filter((request) => request.url === "/api/v1/devices")).toHaveLength(2);
  });

  it("names each app's platform as the site's devices do", async () => {
    for (const [userAgent, platform] of [
      ["Mozilla/5.0 OeeeCafe/ios", "ios"],
      ["Mozilla/5.0 OeeeCafe/macos", "macos"],
    ]) {
      const page = await open({ userAgent, signedIn: true, answers: { "/api/v1/devices": {} } });
      page.window.oeeeApp.pushToken("T");
      expect(JSON.parse(page.asked[0].body).platform, userAgent).toBe(platform);
    }
  });

  it("registers nothing for nobody, and tries again after a failure", async () => {
    const out = await open();
    out.window.oeeeApp.pushToken("T");
    expect(out.asked).toEqual([]);

    const failing = await open({ signedIn: true });
    failing.window.oeeeApp.pushToken("T");
    await settle();
    failing.window.oeeeApp.pushToken("T");
    await settle();
    expect(failing.asked).toHaveLength(2);
  });
});

describe("what a store gives an app", () => {
  /** A site that takes the proof "A" (204) and turns anything else away. */
  function takingOnly(page: Page, taken: string) {
    (window as unknown as { __appContract: Hooks }).__appContract.fetch = (url: string, init?: RequestInit) => {
      page.asked.push({ url, body: String(init?.body ?? "") });
      const proof = new URLSearchParams(String(init?.body)).get("proof");
      return Promise.resolve(new Response(null, { status: proof === taken ? 204 : 404 }));
    };
  }

  it("tells the site each proof, at the build's own store, and answers with the ones it took", async () => {
    const page = await open({ userAgent: "Mozilla/5.0 OeeeCafe/ios", store: "apple" });
    takingOnly(page, "A");
    expect(await page.window.oeeeApp.store.purchased(["A", "B", 3, ""])).toEqual(["A"]);
    expect(page.asked.map((request) => [request.url, request.body])).toEqual([
      ["/store/apple/purchases", "proof=A"],
      ["/store/apple/purchases", "proof=B"],
    ]);
  });

  it("goes the same way for every store", async () => {
    const page = await open({ userAgent: "Mozilla/5.0 OeeeCafe/windows", store: "steam" });
    takingOnly(page, "ab12");
    expect(await page.window.oeeeApp.store.purchased(["ab12"])).toEqual(["ab12"]);
    expect(page.asked.map((request) => request.url)).toEqual(["/store/steam/purchases"]);
  });

  it("takes nothing in a build that sells nowhere, or from nothing", async () => {
    const page = await open();
    expect(await page.window.oeeeApp.store.purchased(["A"])).toEqual([]);
    expect(page.asked).toEqual([]);
    const selling = await open({ store: "apple" });
    expect(await selling.window.oeeeApp.store.purchased(null)).toEqual([]);
  });

  it("gets the Microsoft Store build a ticket and a user for its Store ID key", async () => {
    const told = { ticket: "eyJ0eXAi.eyJhdWQi.c2ln", user: "5f0b6a2e-1d3c-4b7a-9e8f-0a1b2c3d4e5f" };
    const page = await open({
      userAgent: "Mozilla/5.0 Edg/120 OeeeCafe/windows",
      store: "microsoft",
      answers: { "/store/microsoft/tickets": { ...told, extra: "dropped" } },
    });
    expect(await page.window.oeeeApp.store.ticket()).toEqual(told);
    expect(page.asked).toEqual([{ url: "/store/microsoft/tickets", body: "" }]);
  });

  it("gives no ticket where there is none to give", async () => {
    // Signed out, unconfigured or unreachable: the site says anything but 200.
    const refused = await open({ userAgent: "Mozilla/5.0 Edg/120 OeeeCafe/windows", store: "microsoft" });
    expect(await refused.window.oeeeApp.store.ticket()).toBeNull();

    // An answer missing either string is no answer.
    for (const answer of [{ ticket: "t" }, { ticket: "", user: "u" }, { ticket: 1, user: "u" }, null]) {
      const page = await open({
        userAgent: "Mozilla/5.0 Edg/120 OeeeCafe/windows",
        store: "microsoft",
        answers: { "/store/microsoft/tickets": answer },
      });
      expect(await page.window.oeeeApp.store.ticket(), JSON.stringify(answer)).toBeNull();
    }

    // Every other build, and a browser, never asks.
    for (const store of ["apple", "steam", undefined] as const) {
      const page = await open({
        userAgent: "Mozilla/5.0 OeeeCafe/windows",
        store,
        answers: { "/store/microsoft/tickets": { ticket: "t", user: "u" } },
      });
      expect(await page.window.oeeeApp.store.ticket(), String(store)).toBeNull();
      expect(page.asked, String(store)).toEqual([]);
    }
  });

  it("signs in with Steam from the page's own button, in the Steam build only", async () => {
    const page = await open({ userAgent: "Mozilla/5.0 OeeeCafe/windows", store: "steam" });
    const link = page.window.document.querySelector(".auth-steam")!;
    const pressed = new page.window.MouseEvent("click", { bubbles: true, cancelable: true, button: 0 });
    link.dispatchEvent(pressed);
    expect(pressed.defaultPrevented).toBe(true);
    expect(last(page, "signIn")).toEqual({ v: 1, type: "signIn", provider: "steam" });

    // The ticket is posted as a form, not fetched: the site answers with a page.
    const posted: HTMLFormElement[] = [];
    page.window.HTMLFormElement.prototype.submit = function (this: HTMLFormElement) {
      posted.push(this);
    };
    page.window.oeeeApp.signIn.answer({ ticket: "ab12" });
    expect(posted.map((form) => form.getAttribute("action"))).toEqual(["/auth/steam"]);
    expect(Object.fromEntries(new page.window.FormData(posted[0]))).toEqual({ ticket: "ab12", next: "/after" });

    const elsewhere = await open({ userAgent: "Mozilla/5.0 OeeeCafe/windows", store: "microsoft" });
    const there = new elsewhere.window.MouseEvent("click", { bubbles: true, cancelable: true, button: 0 });
    elsewhere.window.addEventListener("click", (event: Event) => event.preventDefault());
    elsewhere.window.document.querySelector(".auth-steam")!.dispatchEvent(there);
    expect(elsewhere.sent.some((message) => message.type === "signIn")).toBe(false);
  });

  it("says so when Steam gives no ticket", async () => {
    const page = await open({ userAgent: "Mozilla/5.0 OeeeCafe/windows", store: "steam" });
    const said: string[] = [];
    page.window.alert = (message?: string) => {
      said.push(String(message));
    };
    page.window.document.querySelector<HTMLElement>(".auth-steam")!.click();
    page.window.oeeeApp.signIn.answer({});
    expect(said).toEqual(["app-steam-sign-in-failed"]);
  });
});

describe("signing in for an app", () => {
  /** Presses a sign-in button, and says whether the page took the press. */
  function press(page: Page, provider: "apple" | "google"): boolean {
    const link = page.window.document.querySelector(`.auth-${provider}`)!;
    let taken = false;
    // Last to hear the click: records whether the page took it, and keeps
    // the test page where it is either way.
    const after = (event: Event) => {
      taken = event.defaultPrevented;
      event.preventDefault();
    };
    page.window.addEventListener("click", after);
    link.dispatchEvent(new page.window.MouseEvent("click", { bubbles: true, cancelable: true, button: 0 }));
    page.window.removeEventListener("click", after);
    return taken;
  }

  it("leaves the app only the answers to call", async () => {
    const page = await open();
    expect(Object.keys(page.window.oeeeApp.signIn).sort()).toEqual(["answer", "resume", "unopened"]);
    expect("restoreContent" in page.window.oeeeApp).toBe(false);
    for (const retired of ["oeeeSignIn", "oeeeCommand", "oeeeRestoreContent", "oeeeStorePrices", "oeeePainter"]) {
      expect(retired in page.window, retired).toBe(false);
    }
  });

  it("takes the press itself, and asks the app for a token for the site's nonce", async () => {
    // Android: Google has Credential Manager.
    const page = await open({ answers: { "/auth/google/start": { state: "S", nonce: "N" } } });
    expect(press(page, "google")).toBe(true);
    await settle();
    await settle();
    expect(Object.fromEntries(new URLSearchParams(page.asked[0].body))).toEqual({ next: "/after" });
    expect(last(page, "signIn")).toEqual({ v: 1, type: "signIn", provider: "google", nonce: "N" });

    page.window.oeeeApp.signIn.answer({ id_token: "T", user: '{"name":{}}' });
    await settle();
    const posted = page.asked.find((request) => request.url === "/auth/google")!;
    expect(Object.fromEntries(new URLSearchParams(posted.body))).toEqual({
      state: "S",
      id_token: "T",
      user: '{"name":{}}',
      format: "json",
    });
  });

  it("asks the app to open the handoff's page in a browser, as an absolute URL", async () => {
    // Android: Apple has no sheet there.
    const page = await open({
      answers: { "/auth/handoff/start": { id: "I", secret: "X", url: "/auth/apple?handoff=I" } },
    });
    expect(press(page, "apple")).toBe(true);
    await settle();
    await settle();
    const message = last(page, "browse");
    expect(keys(message)).toEqual(["type", "url", "v"]);
    expect(message.url).toBe(new URL("/auth/apple?handoff=I", page.window.document.baseURI).href);
    expect(message.url).toMatch(/^https?:\/\/[^/]+\/auth\/apple\?handoff=I$/);
    page.window.oeeeApp.signIn.unopened();
  });

  it("goes the way each app signs in with each provider", async () => {
    const ways: [string, "apple" | "google", string][] = [
      ["Mozilla/5.0 OeeeCafe/ios", "apple", "/auth/apple/start"],
      ["Mozilla/5.0 OeeeCafe/ios", "google", "/auth/google/start"],
      ["Mozilla/5.0 OeeeCafe/macos", "apple", "/auth/apple/start"],
      ["Mozilla/5.0 OeeeCafe/windows", "google", "/auth/handoff/start"],
      ["Mozilla/5.0 OeeeCafe/windows", "apple", "/auth/handoff/start"],
    ];
    for (const [userAgent, provider, asked] of ways) {
      const page = await open({ userAgent });
      expect(press(page, provider), `${userAgent} ${provider}`).toBe(true);
      await settle();
      expect(page.asked[0]?.url, `${userAgent} ${provider}`).toBe(asked);
    }
  });

  it("leaves the buttons to a browser, which does go to the provider", async () => {
    const page = await open({ userAgent: "Mozilla/5.0 Firefox/56.0" });
    expect(press(page, "apple")).toBe(false);
    expect(press(page, "google")).toBe(false);
    expect(page.asked).toEqual([]);
  });
});

describe("what Android's web view cannot do", () => {
  it("shares text through the app", async () => {
    const page = await open();
    await page.window.navigator.share({ title: "A drawing", url: "https://oeee.cafe/@a/1" });
    expect(last(page, "share")).toEqual({ v: 1, type: "share", title: "A drawing", text: "https://oeee.cafe/@a/1" });
  });

  it("hands a download over as a download, whatever the file's type", async () => {
    const page = await open();
    const link = page.window.document.createElement("a");
    link.download = "drawing.png";
    link.href = "data:image/png;base64,iVBORw0KGgo=";
    link.click();
    await new Promise((resolve) => setTimeout(resolve, 50));
    const message = last(page, "download");
    // The file's own type once took the envelope's place, and the app read
    // a message of type "image/png".
    expect(message.type).toBe("download");
    expect(keys(message)).toEqual(["data", "mime", "name", "type", "v"]);
    expect(message).toMatchObject({ name: "drawing.png", mime: "image/png" });
  });

  it("leaves the other apps' web views to do both themselves", async () => {
    const page = await open({ userAgent: "Mozilla/5.0 OeeeCafe/ios" });
    const link = page.window.document.createElement("a");
    link.download = "drawing.png";
    link.href = "data:image/png;base64,iVBORw0KGgo=";
    page.window.document.body.appendChild(link);
    link.addEventListener("click", (event: Event) => event.preventDefault());
    link.click();
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(page.sent.some((message) => message.type === "download")).toBe(false);
  });
});

describe("pulling a page down to reload it", () => {
  /** A page laid out as the site's are: a sticky header, and the content under it. */
  function laidOut(page: Page) {
    const document = page.window.document;
    const layout = document.createElement("div");
    layout.className = "ds-page";
    layout.innerHTML = '<header style="height: 52px">toolbar</header><main class="ds-content">content</main>';
    document.body.appendChild(layout);
    return {
      header: layout.querySelector("header")!,
      content: layout.querySelector("main")!,
    };
  }

  /** A finger put down on `target`, drawn `by` pixels down, and lifted. */
  function pull(page: Page, target: Element, by: number, lift = true) {
    const w = page.window;
    const at = (y: number) => new w.Touch({ identifier: 1, target, clientX: 100, clientY: y });
    const send = (type: string, y: number) => {
      const touch = at(y);
      const event = new w.TouchEvent(type, {
        bubbles: true,
        cancelable: true,
        touches: type === "touchend" ? [] : [touch],
        changedTouches: [touch],
      });
      target.dispatchEvent(event);
      return event;
    };
    send("touchstart", 100);
    const moved = send("touchmove", 100 + by);
    if (lift) send("touchend", 100 + by);
    return moved;
  }

  it("brings the content down from under a still toolbar, with a cucumber between them", async () => {
    const page = await open();
    const { header, content } = laidOut(page);
    const moved = pull(page, content, 120, false);
    // The web view neither scrolls nor bounces while the page is pulled.
    expect(moved.defaultPrevented).toBe(true);
    expect(content.style.transform).toBe("translateY(60px)");
    expect(header.style.transform).toBe("");
    const cucumber = page.window.document.querySelector(".ds-refresh")!;
    expect(cucumber.textContent).toBe("\u{1F952}");
    expect(cucumber.getAttribute("aria-hidden")).toBe("true");
  });

  it("reloads a page let go far enough, spinning the cucumber", async () => {
    const page = await open();
    const { content } = laidOut(page);
    let reloading = false;
    page.window.addEventListener("beforeunload", () => {
      reloading = true;
    });
    pull(page, content, 200);
    expect(reloading).toBe(true);
    expect(page.window.document.querySelector(".ds-refresh")!.classList.contains("is-spinning")).toBe(true);
    expect(last(page, "haptic")).toEqual({ v: 1, type: "haptic", name: "medium" });
  });

  it("puts a page let go too soon back, and reloads nothing", async () => {
    const page = await open();
    const { content } = laidOut(page);
    let reloading = false;
    page.window.addEventListener("beforeunload", () => {
      reloading = true;
    });
    pull(page, content, 60);
    expect(reloading).toBe(false);
    expect(content.style.transform).toBe("");
  });

  it("leaves the painter and replays alone, and the desktop windows and browsers", async () => {
    for (const options of [
      { presence: "drawing" },
      { presence: "watching-replay" },
      { userAgent: "Mozilla/5.0 OeeeCafe/macos" },
      { userAgent: "Mozilla/5.0 OeeeCafe/windows" },
      { userAgent: "Mozilla/5.0 Firefox/56.0" },
    ]) {
      const page = await open(options);
      const { content } = laidOut(page);
      const moved = pull(page, content, 200, false);
      expect(moved.defaultPrevented, JSON.stringify(options)).toBe(false);
      expect(content.style.transform, JSON.stringify(options)).toBe("");
    }
  });

  it("is not a pull that starts in a field, or goes sideways", async () => {
    const page = await open();
    const { content } = laidOut(page);
    const field = page.window.document.createElement("textarea");
    content.appendChild(field);
    expect(pull(page, field, 200, false).defaultPrevented).toBe(false);

    const w = page.window;
    const touch = (x: number, y: number) => new w.Touch({ identifier: 2, target: content, clientX: x, clientY: y });
    content.dispatchEvent(new w.TouchEvent("touchstart", { bubbles: true, touches: [touch(100, 100)] }));
    const sideways = new w.TouchEvent("touchmove", { bubbles: true, cancelable: true, touches: [touch(300, 140)] });
    content.dispatchEvent(sideways);
    expect(sideways.defaultPrevented).toBe(false);
  });
});
