// What the site tells the apps, and the one way it tells them: the channel
// and window.oeeeApp. app_bridge.jinja holds the contract in full -- every
// message, every member, the user agents -- and calls this from the head,
// once a document, with the words the apps say over the page.

/** What an app says in dialogs and menus of its own, in the page's language. */
export type Words = Record<string, string>;

interface Channel {
  postMessage(text: string): void;
}

/** What an app registered, which may not be a channel at all. */
type Registered = Partial<Channel> | undefined;

/** Messages to the apps are objects of plain values, sent as JSON. */
type Data = Record<string, unknown>;

interface DiscordKnown {
  state: unknown;
  friends: unknown;
  requests: unknown;
}

interface SteamKnown {
  friends: unknown;
}

export interface OeeeApp {
  connected(): boolean;
  discord: {
    state(linked: unknown): void;
    friends(list: unknown): void;
    requests(list: unknown): void;
    invited(result: unknown): void;
    known(): DiscordKnown;
    ask(action: string, user?: string): boolean;
  };
  steam: {
    friends(list: unknown): void;
    invited(result: unknown): void;
    known(): SteamKnown;
    ask(action: string, user?: string, join?: string): boolean;
  };
  post(type: string, data: Data, always?: boolean): boolean;
  feel(name: string): void;
  report(): void;
  pushToken(token: unknown): void;
  wouldLoseWork(): boolean;
  leaving(): Promise<void>;
  // Added by the parts of the page that answer an app, as they load.
  [member: string]: unknown;
}

type BridgeWindow = Window &
  typeof globalThis & {
    webkit?: { messageHandlers?: { oeeeBridge?: Registered } };
    oeeeBridge?: Registered;
    oeeeApp?: OeeeApp;
    oeeeLoadingBar?: { leaving(): Promise<void> };
  };

const page = window as BridgeWindow;

/** Opens the channel and makes window.oeeeApp. */
export function installBridge(WORDS: Words): void {
  const root = document.documentElement;
  function channel(): Channel | null {
    const handlers = page.webkit && page.webkit.messageHandlers;
    const target = (handlers && handlers.oeeeBridge) || page.oeeeBridge;
    return target && target.postMessage ? (target as Channel) : null;
  }
  const last: Record<string, string> = {};
  // Sends a message of `type`, unless it is the same as the last one of that
  // type and `always` is not set. Returns whether an app heard it.
  function post(type: string, data: Data, always?: boolean): boolean {
    const target = channel();
    if (!target) return false;
    // The envelope is written last, so a field of the message's own that
    // happens to share its name -- a file's MIME type, say -- can never make
    // it another kind of message.
    const message: Data = {};
    for (const key in data) message[key] = data[key];
    message.v = 1;
    message.type = type;
    const text = JSON.stringify(message);
    // Only what is compared is kept: a download can be megabytes.
    if (!always) {
      if (last[type] === text) return true;
      last[type] = text;
    }
    try {
      target.postMessage(text);
    } catch {
      return false;
    }
    return true;
  }

  // A boosted navigation keeps the head it arrived in (toolbar.jinja), so the
  // presence meta a painter page was loaded with is still there on the page
  // after it. Boosted links only ever lead to pages without one.
  let boosted = false;
  const PAINTING = /^(drawing|relaying|drawing-banner|collaborating)$/;
  function pageState(): Data {
    const meta = boosted ? null : document.querySelector('meta[name="oeee-presence"]');
    const presence = meta ? meta.getAttribute("content") : null;
    const bar = document.querySelector(".nav-bar");
    const join = (meta && meta.getAttribute("data-join")) || null;
    return {
      path: location.pathname,
      signedIn: bar ? bar.hasAttribute("data-signed-in") : null,
      presence,
      community: (meta && meta.getAttribute("data-community")) || null,
      group: (meta && meta.getAttribute("data-group")) || null,
      join,
      seats: join ? parseInt(meta!.getAttribute("data-seats")!, 10) || null : null,
      party: join ? parseInt(meta!.getAttribute("data-party")!, 10) || null : null,
      since: (meta && parseInt(meta.getAttribute("data-since")!, 10)) || null,
      painting: PAINTING.test(presence || ""),
      refreshable: !presence,
    };
  }

  // A design-system colour as the root resolves it now, theme included; null
  // on a page without the design system's stylesheet.
  function token(name: string): string | null {
    const value = getComputedStyle(root).getPropertyValue(name);
    return value ? value.trim() || null : null;
  }
  function themeState(): Data {
    return {
      choice: root.getAttribute("data-theme") || "system",
      ground: token("--ds-ground"),
      grid: token("--ds-grid"),
      toolbar: token("--ds-toolbar"),
    };
  }

  function report(): void {
    if (!channel()) return;
    post("words", WORDS);
    post("page", pageState());
    post("theme", themeState());
    const bell = document.getElementById("nav-notifications");
    if (bell) post("unread", { count: parseInt(bell.getAttribute("data-unread")!, 10) || 0 });
  }

  document.addEventListener("DOMContentLoaded", report);
  // The bell put in place by a live event (live.ts), not by a swap.
  document.addEventListener("oeee:bell", report);
  // Someone came into or left a collaborative room (presence_meta.jinja).
  document.addEventListener("oeee:presence", report);
  window.addEventListener("load", report);
  window.addEventListener("pageshow", (event) => {
    if (event.persisted) boosted = false;
    report();
  });
  window.addEventListener("popstate", () => {
    setTimeout(report, 0);
  });
  document.addEventListener("htmx:after:swap", (event) => {
    const detail = (event as CustomEvent<{ ctx?: { target?: Element } }>).detail;
    const ctx = detail && detail.ctx;
    if (event.target === document.body || (ctx && ctx.target === document.body)) boosted = true;
    report();
  });
  if (window.MutationObserver) {
    new MutationObserver(report).observe(root, { attributes: true, attributeFilter: ["data-theme"] });
  }
  if (window.matchMedia) {
    const scheme = window.matchMedia("(prefers-color-scheme: dark)");
    if (scheme.addEventListener) scheme.addEventListener("change", report);
    else if (scheme.addListener) scheme.addListener(report);
  }

  // Whether a drag to the right from `target` is the page's: a slider, an
  // element that keeps horizontal pans from the browser (a drawing zoomed
  // closer, postZoom.ts), or a row scrolled along that has room to scroll
  // back. A row at its start is not: the drag has nothing there to move.
  const PANS_SIDEWAYS = /^(auto|manipulation)$|pan-x|pan-left|pan-right/;
  function takesSideways(target: Element | null): boolean {
    for (let element = target; element && element.nodeType === 1; element = element.parentElement) {
      if (element.matches('input[type="range"]')) return true;
      const style = window.getComputedStyle(element);
      if (!PANS_SIDEWAYS.test(style.touchAction || "auto")) return true;
      if (
        element.scrollLeft > 0 &&
        element.scrollWidth > element.clientWidth &&
        /auto|scroll/.test(style.overflowX)
      ) {
        return true;
      }
    }
    const scroller = document.scrollingElement;
    return !!scroller && scroller.scrollLeft > 0;
  }

  // Which drawing a finger lands on, and whether a sideways drag from there
  // is the page's, said as it lands: by the time a long press or a swipe is
  // recognised the app can no longer ask the page in time.
  window.addEventListener(
    "touchstart",
    (event) => {
      if (!channel()) return;
      const target = event.target as Element | null;
      const image = target && target.closest ? target.closest<HTMLImageElement>("img[data-oeee-drawing]") : null;
      const link = image ? (image.closest("a[href]") as HTMLAnchorElement | null) : null;
      post(
        "pressed",
        {
          drawing: image
            ? {
                src: image.currentSrc || image.src,
                link: link ? link.href : "",
                width: image.naturalWidth || image.width,
                height: image.naturalHeight || image.height,
              }
            : null,
          sideways: takesSideways(target),
        },
        true,
      );
    },
    { capture: true, passive: true },
  );

  // The push token the app handed over, registered for whoever the page is
  // signed in as. Once a document: signing in or out loads a new one, and the
  // account signed in then takes the token over.
  let registered: string | null = null;
  const PLATFORMS: Record<string, string> = { ios: "ios", android: "android", macos: "macos" };
  function pushToken(token: unknown): void {
    if (typeof token !== "string" || !token || registered === token) return;
    const bar = document.querySelector(".nav-bar");
    if (!bar || !bar.hasAttribute("data-signed-in")) return;
    const app = root.getAttribute("data-app") || "";
    const platform = Object.prototype.hasOwnProperty.call(PLATFORMS, app) ? PLATFORMS[app] : undefined;
    if (!platform) return;
    registered = token;
    fetch("/devices", {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ device_token: token, platform }),
    }).then(
      (response) => {
        if (!response.ok) registered = null;
      },
      () => {
        registered = null;
      },
    );
  }

  // The reader's Discord account in the Windows app, as it last said, for
  // whatever part of the page asks after it was told.
  const discord: DiscordKnown = { state: null, friends: null, requests: null };
  function heardFromDiscord(kind: "state" | "friends" | "requests" | "invited", value: unknown): void {
    if (kind !== "invited") discord[kind] = value;
    document.dispatchEvent(new CustomEvent("oeee:discord", { detail: { kind, value } }));
  }

  // The Steam friends the Steam build last listed, for whatever part of the
  // page asks after it was told.
  const steam: SteamKnown = { friends: null };
  function heardFromSteam(kind: "friends" | "invited", value: unknown): void {
    if (kind === "friends") steam.friends = value;
    document.dispatchEvent(new CustomEvent("oeee:steam", { detail: { kind, value } }));
  }

  page.oeeeApp = {
    // Whether an app is listening at all.
    connected: () => !!channel(),
    discord: {
      state: (linked) => heardFromDiscord("state", linked),
      friends: (list) => heardFromDiscord("friends", list),
      requests: (list) => heardFromDiscord("requests", list),
      invited: (result) => heardFromDiscord("invited", result),
      known: () => discord,
      ask: (action, user) => {
        const data: Data = { action };
        if (user) data.user = user;
        return post("discord", data, true);
      },
    },
    steam: {
      friends: (list) => heardFromSteam("friends", list),
      invited: (result) => heardFromSteam("invited", result),
      known: () => steam,
      ask: (action, user, join) => {
        const data: Data = { action };
        if (user) data.user = user;
        if (join) data.join = join;
        return post("steam", data, true);
      },
    },
    post,
    // A control felt as well as seen.
    feel: (name) => {
      if (name) post("haptic", { name }, true);
    },
    // Said again after something the page changed by itself.
    report,
    pushToken,
    // Whether the page would stop a browser from leaving it. Its value is the
    // answer, so an app evaluates the call and reads it.
    wouldLoseWork: () => {
      let event: Event;
      try {
        event = document.createEvent("BeforeUnloadEvent");
        event.initEvent("beforeunload", false, true);
      } catch {
        event = new Event("beforeunload", { cancelable: true });
      }
      window.dispatchEvent(event);
      const returnValue = (event as BeforeUnloadEvent).returnValue as unknown;
      return event.defaultPrevented || (typeof returnValue === "string" && returnValue !== "");
    },
    // The reader said Leave in the app's own dialog, and the app is about to
    // load the next page: the loading bar goes up (loadingBar.ts), and the
    // promise settles when the load may begin.
    leaving: () => {
      const bar = page.oeeeLoadingBar;
      return bar ? bar.leaving() : Promise.resolve();
    },
  };
}
