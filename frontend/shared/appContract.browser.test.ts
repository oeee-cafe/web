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
    oeeeApp: { report(): void; feel(name: string): void; wouldLoseWork(): boolean };
    oeeeSignIn: {
      native(provider: string, next: string | null): void;
      answer(told: Record<string, unknown>): void;
      browser(provider: string, next: string | null): void;
      unopened(): void;
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
      options.userAgent ?? "Mozilla/5.0 OeeeCafeAndroid",
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
       src="data:image/gif;base64,R0lGODlhAQABAAAAACw="></a>`;
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

  it("gives the theme, the colours at the edges, and the design system's ground", async () => {
    const page = await open();
    const message = last(page, "theme");
    expect(keys(message)).toEqual(["bottom", "choice", "dark", "grid", "ground", "top", "type", "v"]);
    expect(message).toMatchObject({
      choice: "system",
      top: "rgb(250, 250, 252)",
      bottom: "rgb(255, 255, 255)",
      ground: "#ccccff",
      grid: "#bbbbff",
    });
    expect(typeof message.dark).toBe("boolean");
  });

  it("gives every word an app says over the page, as strings", async () => {
    const page = await open();
    const message = last(page, "words");
    expect(keys(message)).toEqual(
      [
        "cancel", "copyImage", "copyLink", "leave", "leaveBody", "leaveTitle", "ok", "saveFailed",
        "saveImage", "savedFile", "savedImage", "share", "stay", "steamSignInFailed", "type", "v",
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

  it("marks the root for the app the user agent names", async () => {
    const cases: [string, string, string][] = [
      ["Mozilla/5.0 OeeeCafeiOS", "data-app", "ios"],
      ["Mozilla/5.0 OeeeCafeAndroid", "data-app", "android"],
      ["Mozilla/5.0 OeeeCafeMac", "data-desktop", "macos"],
      ["Mozilla/5.0 Edg/120 OeeeCafeWindows", "data-desktop", "windows"],
    ];
    for (const [userAgent, attribute, value] of cases) {
      const page = await open({ userAgent });
      expect(page.window.document.documentElement.getAttribute(attribute), userAgent).toBe(value);
    }
  });
});

describe("signing in for an app", () => {
  it("asks for a token for the site's nonce, and posts what the app answers", async () => {
    const page = await open({ answers: { "/auth/google/start": { state: "S", nonce: "N" } } });
    page.window.oeeeSignIn.native("google", "/after");
    await settle();
    await settle();
    expect(last(page, "signIn")).toEqual({ v: 1, type: "signIn", provider: "google", nonce: "N" });

    page.window.oeeeSignIn.answer({ id_token: "T", user: '{"name":{}}' });
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
    const page = await open({
      answers: { "/auth/handoff/start": { id: "I", secret: "X", url: "/auth/apple?handoff=I" } },
    });
    page.window.oeeeSignIn.browser("apple", "/after");
    await settle();
    await settle();
    const message = last(page, "browse");
    expect(keys(message)).toEqual(["type", "url", "v"]);
    expect(message.url).toBe(new URL("/auth/apple?handoff=I", page.window.document.baseURI).href);
    expect(message.url).toMatch(/^https?:\/\/[^/]+\/auth\/apple\?handoff=I$/);
    page.window.oeeeSignIn.unopened();
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
    const page = await open({ userAgent: "Mozilla/5.0 OeeeCafeiOS" });
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
