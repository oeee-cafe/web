/**
 * What the site says to the apps, checked against the shape the apps read.
 *
 * The apps -- oeee-cafe-apple, oeee-cafe-android and oeee-cafe-desktop --
 * parse every message the page sends on oeeeBridge (app_bridge.jinja) and
 * drop one they cannot read without a word, so a field renamed or retyped
 * here would break them with nothing failing on this side. They each used to
 * keep a copy of captured messages to test against, copied by hand and never
 * recaptured. So the copy they test against now is appContract.json, and the
 * last part of this file captures the page again and fails when that file no
 * longer says what the page does: a change the apps must follow is a change
 * to that file, and each app fetches it with a script of its own.
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
import { offerPainterToApp } from "./appBridge";
import contract from "./appContract.json";

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
    )
    .replace(/\{\{\s*ftl_get_message\("([^"]+)"\)\s*\}\}/g, (_, id: string) => id);
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
  /** The toolbar carries the Windows app's caption (app_caption.jinja), as the site's does. */
  caption?: boolean;
  /** The page is /supporter, with a button for each of these products (supporter.jinja). */
  supporter?: string[];
  /** The page has the loading bar, as every page with the toolbar has (loading_bar.jinja). */
  loadingBar?: boolean;
}

/** /supporter's own script, which is all of it that talks to an app. */
function supporterScript(): string {
  const scripts = template("supporter.jinja").match(/<script>[\s\S]*?<\/script>/g) ?? [];
  const script = scripts.pop();
  if (!script || /\{[{%]/.test(script)) throw new Error("supporter.jinja's script is not plain");
  return script;
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
      (options.userAgent ?? "Mozilla/5.0 OeeeCafe platform/android") + (options.store ? ` store/${options.store}` : ""),
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
      ${options.caption ? template("app_caption.jinja") : ""}
    </nav>
    <a href="/@artist/9c881320"><img data-oeee-drawing width="300" height="200"
       src="/image/9c881320.png"></a>
    <a class="auth-apple" href="/auth/apple?next=%2Fafter">Apple</a>
    <a class="auth-google" href="/auth/google?next=%2Fafter">Google</a>
    <a class="auth-steam" href="/auth/steam/app?next=%2Fafter">Steam</a>
    ${(options.supporter ?? [])
      .map((product) => `<button class="supporter-buy" data-product="${product}"></button>`)
      .join("")}
    ${options.supporter ? `<button class="supporter-restore"></button>${supporterScript()}` : ""}
    ${options.loadingBar ? template("loading_bar.jinja") : ""}`;
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
  it("asks the Windows app to minimise, maximise and close, and shows what it says back", async () => {
    const page = await open({ userAgent: "Mozilla/5.0 OeeeCafe platform/windows", caption: true });
    const doc = page.window.document;
    const button = (name: string) => doc.querySelector<HTMLButtonElement>(`.oeee-caption .is-${name}`)!;
    button("minimize").click();
    button("maximize").click();
    button("close").click();
    const window = page.sent.filter((message) => message.type === "window");
    expect(window.map((message) => message.action)).toEqual(["minimize", "maximize", "close"]);

    // Where its maximise button is, for the app's Snap Layouts stand-in.
    await new Promise((resolve) => page.window.requestAnimationFrame(resolve));
    const caption = last(page, "caption");
    expect(keys(caption)).toEqual(["place", "type", "v"]);
    const place = caption.place as Record<string, number>;
    expect(place.width).toBeGreaterThan(0);

    const told = (page.window.oeeeApp as unknown as { caption(told: unknown): void }).caption;
    told({ maximized: true, pointer: "hot" });
    expect(button("maximize").getAttribute("aria-label")).toBe("window-restore");
    expect(button("maximize").classList.contains("is-hot")).toBe(true);
    told({ maximized: false, pointer: "" });
    expect(button("maximize").getAttribute("aria-label")).toBe("window-maximize");
    expect(button("maximize").classList.contains("is-hot")).toBe(false);

    // A press on them is theirs, not a drag of the window.
    const press = new page.window.MouseEvent("mousedown", { bubbles: true, cancelable: true, button: 0 });
    let reached = false;
    doc.addEventListener("mousedown", () => (reached = true));
    button("close").dispatchEvent(press);
    expect(reached).toBe(false);

    // Hidden and silent everywhere else.
    const mac = await open({ userAgent: "Mozilla/5.0 OeeeCafe platform/macos store/apple", caption: true });
    mac.window.document.querySelector<HTMLButtonElement>(".oeee-caption .is-close")!.click();
    expect(mac.sent.some((message) => message.type === "window" || message.type === "caption")).toBe(false);
  });

  it("moves and zooms the Mac window from the toolbar, and keeps the browser's menu quiet", async () => {
    const page = await open({ userAgent: "Mozilla/5.0 OeeeCafe platform/macos store/apple" });
    const bar = page.window.document.querySelector(".nav-bar")!;
    const press = (detail: number) => {
      const event = new page.window.MouseEvent("mousedown", { bubbles: true, cancelable: true, button: 0, detail });
      bar.dispatchEvent(event);
      return event.defaultPrevented;
    };
    expect(press(1)).toBe(true);
    expect(press(1)).toBe(true);
    expect(press(2)).toBe(true);
    const window = page.sent.filter((message) => message.type === "window");
    // Every press is heard, the same one twice included.
    expect(window.map((message) => message.action)).toEqual(["drag", "drag", "zoom"]);
    expect(keys(window[0])).toEqual(["action", "type", "v"]);

    // A link on the bar is the page's, not the window's.
    const link = page.window.document.querySelector("#nav-notifications")!;
    link.dispatchEvent(new page.window.MouseEvent("mousedown", { bubbles: true, cancelable: true, button: 0 }));
    expect(page.sent.filter((message) => message.type === "window")).toHaveLength(3);

    const menu = (target: Element) => {
      const event = new page.window.MouseEvent("contextmenu", { bubbles: true, cancelable: true });
      target.dispatchEvent(event);
      return event.defaultPrevented;
    };
    expect(menu(bar)).toBe(true);
    expect(menu(page.window.document.querySelector("img")!)).toBe(false);

    // Not in a browser, nor in the other apps.
    const browser = await open({ userAgent: "Mozilla/5.0" });
    const plain = new browser.window.MouseEvent("contextmenu", { bubbles: true, cancelable: true });
    browser.window.document.querySelector(".nav-bar")!.dispatchEvent(plain);
    expect(plain.defaultPrevented).toBe(false);
  });

  it("says who is signed in, what they are doing, and what they may do", async () => {
    const page = await open({ signedIn: true, presence: "collaborating" });
    const message = last(page, "page");
    expect(keys(message)).toEqual(
      ["community", "group", "painting", "path", "presence", "refreshable", "signedIn", "type", "v"],
    );
    expect(message).toMatchObject({
      v: 1,
      signedIn: true,
      presence: "collaborating",
      community: '오이카페 "모에화"',
      group: "0123456789abcdef",
      painting: true,
      refreshable: false,
    });
    expect(typeof message.path).toBe("string");
  });

  it("cannot say who is signed in on a page without the toolbar, and says so", async () => {
    const page = await open();
    page.window.document.querySelector(".nav-bar")!.remove();
    page.window.oeeeApp.report();
    expect(last(page, "page")).toMatchObject({ signedIn: null, presence: null, painting: false, refreshable: true });
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
        "copyImage", "copyLink", "leave", "leaveBody", "leaveTitle", "saveFailed",
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
    // The cases the apps build their user agents against, and the server
    // reads its store from (Store::from_user_agent): only these names, only
    // as words of their own, and the platform and store in one run, either
    // way round.
    const marks = (root: HTMLElement) =>
      ["data-app", "data-form", "data-store"].map((name) => root.getAttribute(name));
    for (const { agent, app, form, store } of contract.userAgents) {
      const page = await open({ userAgent: agent });
      expect(marks(page.window.document.documentElement), agent).toEqual([app, form, store]);
    }
  });

  it("sends haptics to any app listening, whatever it is on", async () => {
    const page = await open({ userAgent: "Mozilla/5.0 OeeeCafe platform/macos" });
    const button = page.window.document.createElement("button");
    button.setAttribute("data-haptic", "light");
    page.window.document.body.appendChild(button);
    button.click();
    expect(last(page, "haptic")).toEqual({ v: 1, type: "haptic", name: "light" });
  });
});

describe("push notifications for an app", () => {
  it("registers the app's token for whoever is signed in, once, with the platform its user agent names", async () => {
    const page = await open({ signedIn: true, answers: { "/devices": { id: "D" } } });
    page.window.oeeeApp.pushToken("T1");
    page.window.oeeeApp.pushToken("T1");
    await settle();
    const posted = page.asked.filter((request) => request.url === "/devices");
    expect(posted.map((request) => JSON.parse(request.body))).toEqual([{ device_token: "T1", platform: "android" }]);

    // A token the app was given since is registered too.
    page.window.oeeeApp.pushToken("T2");
    await settle();
    expect(page.asked.filter((request) => request.url === "/devices")).toHaveLength(2);
  });

  it("names each app's platform as the site's devices do", async () => {
    for (const [userAgent, platform] of [
      ["Mozilla/5.0 OeeeCafe platform/ios", "ios"],
      ["Mozilla/5.0 OeeeCafe platform/macos", "macos"],
    ]) {
      const page = await open({ userAgent, signedIn: true, answers: { "/devices": {} } });
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
    const page = await open({ userAgent: "Mozilla/5.0 OeeeCafe platform/ios", store: "apple" });
    takingOnly(page, "A");
    expect(await page.window.oeeeApp.store.purchased(["A", "B", 3, ""])).toEqual(["A"]);
    expect(page.asked.map((request) => [request.url, request.body])).toEqual([
      ["/store/apple/purchases", "proof=A"],
      ["/store/apple/purchases", "proof=B"],
    ]);
  });

  it("goes the same way for every store", async () => {
    const page = await open({ userAgent: "Mozilla/5.0 OeeeCafe platform/windows", store: "steam" });
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
      userAgent: "Mozilla/5.0 Edg/120 OeeeCafe platform/windows",
      store: "microsoft",
      answers: { "/store/microsoft/tickets": { ...told, extra: "dropped" } },
    });
    expect(await page.window.oeeeApp.store.ticket()).toEqual(told);
    expect(page.asked).toEqual([{ url: "/store/microsoft/tickets", body: "" }]);
  });

  it("gives no ticket where there is none to give", async () => {
    // Signed out, unconfigured or unreachable: the site says anything but 200.
    const refused = await open({ userAgent: "Mozilla/5.0 Edg/120 OeeeCafe platform/windows", store: "microsoft" });
    expect(await refused.window.oeeeApp.store.ticket()).toBeNull();

    // An answer missing either string is no answer.
    for (const answer of [{ ticket: "t" }, { ticket: "", user: "u" }, { ticket: 1, user: "u" }, null]) {
      const page = await open({
        userAgent: "Mozilla/5.0 Edg/120 OeeeCafe platform/windows",
        store: "microsoft",
        answers: { "/store/microsoft/tickets": answer },
      });
      expect(await page.window.oeeeApp.store.ticket(), JSON.stringify(answer)).toBeNull();
    }

    // Every other build, and a browser, never asks.
    for (const store of ["apple", "steam", undefined] as const) {
      const page = await open({
        userAgent: "Mozilla/5.0 OeeeCafe platform/windows",
        store,
        answers: { "/store/microsoft/tickets": { ticket: "t", user: "u" } },
      });
      expect(await page.window.oeeeApp.store.ticket(), String(store)).toBeNull();
      expect(page.asked, String(store)).toEqual([]);
    }
  });

  it("signs in with Steam from the page's own button, in the Steam build only", async () => {
    const page = await open({ userAgent: "Mozilla/5.0 OeeeCafe platform/windows", store: "steam" });
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

    const elsewhere = await open({ userAgent: "Mozilla/5.0 OeeeCafe platform/windows", store: "microsoft" });
    const there = new elsewhere.window.MouseEvent("click", { bubbles: true, cancelable: true, button: 0 });
    elsewhere.window.addEventListener("click", (event: Event) => event.preventDefault());
    elsewhere.window.document.querySelector(".auth-steam")!.dispatchEvent(there);
    expect(elsewhere.sent.some((message) => message.type === "signIn")).toBe(false);
  });

  it("says so when Steam gives no ticket", async () => {
    const page = await open({ userAgent: "Mozilla/5.0 OeeeCafe platform/windows", store: "steam" });
    const said: string[] = [];
    page.window.alert = (message?: string) => {
      said.push(String(message));
    };
    page.window.document.querySelector<HTMLElement>(".auth-steam")!.click();
    page.window.oeeeApp.signIn.answer({});
    expect(said).toEqual(["app-steam-sign-in-failed"]);
  });

  it("says it in the site's own alert where the page has one", async () => {
    const page = await open({ userAgent: "Mozilla/5.0 OeeeCafe platform/windows", store: "steam" });
    const said: string[] = [];
    const site = page.window as unknown as { dsAlert: (message: string) => void };
    site.dsAlert = (message: string) => said.push(message);
    page.window.alert = () => {
      throw new Error("the browser's alert");
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

  it("asks before linking in the site's own dialog, and asks the site nothing more until answered", async () => {
    const page = await open({
      answers: {
        "/auth/handoff/start": { id: "I", secret: "X", url: "/auth/apple?handoff=I" },
        "/auth/handoff/claim": { status: "confirm", message: "Link these?" },
      },
    });
    const questions: [string, string, string][] = [];
    let answer: (yes: boolean) => void = () => {};
    const site = page.window as unknown as {
      dsConfirm: (text: string, action: string, tone: string) => Promise<boolean>;
    };
    site.dsConfirm = (text, action, tone) => {
      questions.push([text, action, tone]);
      return new Promise<boolean>((resolve) => (answer = resolve));
    };
    page.window.confirm = () => {
      throw new Error("the browser's confirm");
    };
    const claims = () => page.asked.filter((request) => request.url.includes("/auth/handoff/claim"));
    expect(press(page, "apple")).toBe(true);
    // The page asks whether the browser has finished every two seconds.
    await new Promise((resolve) => setTimeout(resolve, 2300));
    expect(questions).toEqual([["Link these?", "ok", "plain"]]);
    expect(claims()).toHaveLength(1);
    // Left open through another round: not asked again, nor the site.
    await new Promise((resolve) => setTimeout(resolve, 2300));
    expect(questions).toHaveLength(1);
    expect(claims()).toHaveLength(1);
    answer(true);
    await settle();
    await settle();
    expect(claims()).toHaveLength(2);
    expect(Object.fromEntries(new URLSearchParams(claims()[1].body))).toMatchObject({ confirm: "1" });
    page.window.oeeeApp.signIn.unopened();
  }, 10000);

  it("goes the way each app signs in with each provider", async () => {
    const ways: [string, "apple" | "google", string][] = [
      ["Mozilla/5.0 OeeeCafe platform/ios", "apple", "/auth/apple/start"],
      ["Mozilla/5.0 OeeeCafe platform/ios", "google", "/auth/handoff/start"],
      ["Mozilla/5.0 OeeeCafe platform/macos", "google", "/auth/handoff/start"],
      ["Mozilla/5.0 OeeeCafe platform/macos", "apple", "/auth/apple/start"],
      ["Mozilla/5.0 OeeeCafe platform/windows", "google", "/auth/handoff/start"],
      ["Mozilla/5.0 OeeeCafe platform/windows", "apple", "/auth/handoff/start"],
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
    const page = await open({ userAgent: "Mozilla/5.0 OeeeCafe platform/ios" });
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


describe("what the apps test against (appContract.json)", () => {
  /** Where the test's own pages are, which the site's would be at. */
  const site = (value: unknown) =>
    JSON.parse(JSON.stringify(value).split(location.origin).join("https://oeee.cafe"));

  /** Every message the page sends, from the pages that send each. */
  async function captured(): Promise<Record<string, Message[]>> {
    const sent: Message[] = [];
    const keep = (page: Page) => page.sent;

    const signedIn = await open({ signedIn: true, presence: "collaborating", unread: 12 });
    sent.push(...keep(signedIn));
    const bare = await open();
    bare.window.document.querySelector(".nav-bar")!.remove();
    bare.window.oeeeApp.report();
    sent.push(last(bare, "page"));

    bare.window.oeeeApp.feel("success");
    bare.window.document.querySelector("img")!.dispatchEvent(new bare.window.Event("touchstart", { bubbles: true }));
    bare.window.document.body.dispatchEvent(new bare.window.Event("touchstart", { bubbles: true }));
    sent.push(...bare.sent.filter((message) => message.type === "haptic" || message.type === "pressed"));

    // The painter, offered from the drawing page's own module.
    const drawing = await open();
    const host = window as unknown as { oeeeApp?: unknown };
    host.oeeeApp = drawing.window.oeeeApp;
    try {
      offerPainterToApp({ command() {}, preferPen() {} } as unknown as Parameters<typeof offerPainterToApp>[0]);
    } finally {
      delete host.oeeeApp;
    }
    sent.push(last(drawing, "painter"));

    const supporter = await open({
      userAgent: "Mozilla/5.0 OeeeCafe platform/ios",
      store: "apple",
      supporter: ["cafe.oeee.supporter.2026"],
    });
    supporter.window.document.querySelector<HTMLElement>(".supporter-buy")!.click();
    supporter.window.document.querySelector<HTMLElement>(".supporter-restore")!.click();
    sent.push(...supporter.sent.filter((message) => /^(prices|purchase|restore)$/.test(message.type)));

    const google = await open({ answers: { "/auth/google/start": { state: "S", nonce: "N" } } });
    google.window.document.querySelector<HTMLElement>(".auth-google")!.dispatchEvent(
      new google.window.MouseEvent("click", { bubbles: true, cancelable: true, button: 0 }),
    );
    await settle();
    await settle();
    sent.push(last(google, "signIn"));

    const steam = await open({ userAgent: "Mozilla/5.0 OeeeCafe platform/windows", store: "steam" });
    steam.window.document.querySelector(".auth-steam")!.dispatchEvent(
      new steam.window.MouseEvent("click", { bubbles: true, cancelable: true, button: 0 }),
    );
    sent.push(last(steam, "signIn"));

    const handoff = await open({
      answers: { "/auth/handoff/start": { id: "I", secret: "X", url: "/auth/apple?handoff=I" } },
    });
    handoff.window.addEventListener("click", (event: Event) => event.preventDefault());
    handoff.window.document.querySelector(".auth-apple")!.dispatchEvent(
      new handoff.window.MouseEvent("click", { bubbles: true, cancelable: true, button: 0 }),
    );
    await settle();
    await settle();
    sent.push(last(handoff, "browse"));
    handoff.window.oeeeApp.signIn.unopened();

    const android = await open();
    await android.window.navigator.share({ title: "A drawing", url: "https://oeee.cafe/@a/1" });
    const file = android.window.document.createElement("a");
    file.download = "drawing.png";
    file.href = "data:image/png;base64,iVBORw0KGgo=";
    file.click();
    await new Promise((resolve) => setTimeout(resolve, 50));
    sent.push(last(android, "share"), last(android, "download"));

    const mac = await open({ userAgent: "Mozilla/5.0 OeeeCafe platform/macos store/apple" });
    mac.window.document.querySelector(".nav-bar")!.dispatchEvent(
      new mac.window.MouseEvent("mousedown", { bubbles: true, cancelable: true, button: 0, detail: 1 }),
    );
    sent.push(last(mac, "window"));

    const windows = await open({ userAgent: "Mozilla/5.0 OeeeCafe platform/windows", caption: true });
    windows.window.document.querySelector<HTMLElement>(".oeee-caption .is-maximize")!.click();
    await new Promise((resolve) => windows.window.requestAnimationFrame(resolve));
    sent.push(last(windows, "window"), last(windows, "caption"));

    // One example of each distinct message, in the order first sent.
    const byType: Record<string, Message[]> = {};
    for (const message of site(sent) as Message[]) {
      const same = (byType[message.type] ??= []);
      if (!same.some((kept) => JSON.stringify(kept) === JSON.stringify(message))) same.push(message);
    }
    return byType;
  }

  /** What a message is made of, which is what the caption's pixels are held to. */
  function shape(value: unknown): unknown {
    if (value === null) return null;
    if (Array.isArray(value)) return value.map(shape);
    if (typeof value === "object") {
      return Object.fromEntries(Object.entries(value as object).map(([key, inner]) => [key, shape(inner)]));
    }
    return typeof value;
  }

  it("is an example of every message the page sends, as it sends it", async () => {
    const examples = contract.messages as Record<string, Message[]>;
    const sent = await captured();
    expect(Object.keys(sent).sort()).toEqual(Object.keys(examples).sort());
    for (const [type, messages] of Object.entries(sent)) {
      // Where the Windows caption's button is depends on how this browser
      // lays it out; what the app reads from it does not.
      if (type === "caption") expect(messages.map(shape), type).toEqual(examples[type].map(shape));
      else expect(messages, type).toEqual(examples[type]);
    }
    // Every example says it is this contract's.
    for (const messages of Object.values(examples)) {
      for (const message of messages) expect(message.v).toBe(contract.v);
    }
  });

  it("names every member an app may call, and each is there", async () => {
    const page = await open({
      userAgent: "Mozilla/5.0 OeeeCafe platform/windows",
      store: "microsoft",
      caption: true,
      supporter: ["cafe.oeee.supporter.2026"],
    });
    const host = window as unknown as { oeeeApp?: unknown };
    host.oeeeApp = page.window.oeeeApp;
    try {
      offerPainterToApp({ command() {}, preferPen() {} } as unknown as Parameters<typeof offerPainterToApp>[0]);
    } finally {
      delete host.oeeeApp;
    }
    // The toolbar's commands need the server to render it; the Rust tests
    // hold it to adding `command` (the_apps_are_told_the_unread_count_and_who_is_signed_in),
    // and its table is read here for the names it takes.
    const toolbar = template("toolbar.jinja");
    expect(toolbar).toContain("window.oeeeApp.command = command;");
    const table = /var commands = \{([\s\S]*?)\n\s*\};/.exec(toolbar)![1];
    const named = [...table.matchAll(/^\s*"([a-z-]+)": function/gm)].map((match) => match[1]);
    expect(named.sort()).toEqual([...contract.commands].sort());
    const found = (path: string) =>
      path.split(".").reduce<unknown>((at, name) => (at as Record<string, unknown> | undefined)?.[name], page.window.oeeeApp);
    for (const member of contract.members) {
      if (member === "command") continue;
      expect(typeof found(member), member).toBe("function");
    }
  });

  it("asks whether leaving would lose work, and shows the page is being left, in the words every app uses", async () => {
    const page = await open();
    const run = (script: string) => page.window.eval(script) as unknown;
    expect(run(contract.scripts.wouldLoseWork)).toBe(false);
    page.window.addEventListener("beforeunload", (event: Event) => event.preventDefault());
    expect(run(contract.scripts.wouldLoseWork)).toBe(true);
    await expect(run(contract.scripts.leaving)).resolves.toBeUndefined();

    // With the loading bar, as every page with the toolbar has it: up as the
    // app is told it may go, on WebKit a frame before (loading_bar.jinja).
    for (const app of ["ios", "android"]) {
      const barred = await open({ userAgent: `Mozilla/5.0 OeeeCafe platform/${app}`, loadingBar: true });
      const left = barred.window.eval(contract.scripts.leaving) as Promise<void>;
      const bar = barred.window.document.querySelector(".ds-loading-bar");
      expect(bar?.className, app).toContain("is-loading");
      expect(bar?.className.includes("is-now"), app).toBe(app === "ios");
      await expect(left, app).resolves.toBeUndefined();
    }

    // A page that is not the site's -- the desktop app's loader, a page
    // outside -- answers without throwing.
    const outside = document.createElement("iframe");
    frames.push(outside);
    document.body.appendChild(outside);
    const plain = outside.contentWindow as Window & typeof globalThis;
    expect(plain.eval(contract.scripts.wouldLoseWork)).toBe(false);
    expect(plain.eval(contract.scripts.leaving)).toBeNull();
  });
});
