// The loading bar (loading_bar.jinja): a thin line under the header while
// the next page is on its way, so a tap is seen to have been taken.
//
// Shown in browsers and apps, for ordinary page loads and htmx navigation.
//
// The bar hangs from the header's bottom edge, inside it, so it goes wherever
// the header goes -- pinned, pulled down with a refresh, scrolled under the
// status bar. Fixed to the screen instead, it parted from the header as soon
// as either moved: iOS insets the page under the status bar, and a sticky
// header and a fixed bar do not agree on where the top is. A boosted swap
// puts a new header up mid-trip, and the bar is carried over into it where it
// had got to. It creeps towards the end and never reaches it on its own
// (ds.css, "The loading bar"); what finishes it is the page arriving --
// htmx's swap, or for a page load, the new document -- or, for a trip that
// never lands (a download, a cancelled load), a timeout.
//
// Clicks and submits start it at the press. In browsers and apps built on
// WebKit, a press that loads a page holds the load back a frame, until the
// bar is on the screen (leave(), below). Any page load that is neither -- a
// script's, or back to a page not kept -- is caught as the page is about to
// be left (beforeunload, below). htmx's own trips are only ever the first
// kind.

interface Ctx {
  sourceElement?: Element;
  status?: string;
}

type LoadingBarWindow = Window &
  typeof globalThis & {
    htmx?: unknown;
    oeeeApp?: { wouldLoseWork?: () => boolean };
    oeeeLoadingBar?: {
      start(from?: Element | null, now?: boolean): void;
      done(): void;
      leave(go: () => void): void;
      leaving(): Promise<void>;
    };
  };

const page = window as LoadingBarWindow;

function ctxOf(event: Event): Ctx | undefined {
  const detail = (event as CustomEvent<{ ctx?: Ctx }>).detail;
  return detail && detail.ctx;
}

/** Sets the bar up, once for the document. */
export function installLoadingBar(): void {
  if (page.oeeeLoadingBar) return;

  let bar: HTMLDivElement | null = null;
  let source: Element | null = null;
  let request: Ctx | null = null;
  let sent: Ctx | null = null;
  let timeout: ReturnType<typeof setTimeout> | undefined;
  let clearing: ReturnType<typeof setTimeout> | undefined;

  function reset(): void {
    clearTimeout(timeout);
    clearTimeout(clearing);
    source = null;
    request = null;
    if (bar) bar.className = "ds-loading-bar";
  }
  function loadingNow(): boolean {
    return !!bar && bar.className.indexOf("is-loading") >= 0;
  }
  // Under the whole header, the notices included; the painter pages have the
  // toolbar alone.
  function hang(): boolean {
    const header = document.querySelector(".ds-page > header") || document.querySelector(".nav-bar");
    if (!header) return false;
    if (bar!.parentNode !== header) header.appendChild(bar!);
    return true;
  }
  function start(from?: Element | null, now?: boolean): void {
    if (navigator.onLine === false) return;
    if (!bar) {
      bar = document.createElement("div");
      bar.setAttribute("aria-hidden", "true");
    }
    reset();
    if (!hang()) return;
    // Read back once, so the step to .is-loading is a transition from nothing
    // rather than from wherever the last trip left it.
    void bar.offsetWidth;
    bar.className = "ds-loading-bar is-loading" + (now ? " is-now" : "");
    source = from || null;
    if (source && sent && sent.sourceElement === source && sent.status === "issuing") request = sent;
    timeout = setTimeout(done, 20000);
  }
  function done(): void {
    if (!loadingNow()) return;
    clearTimeout(timeout);
    source = null;
    request = null;
    bar!.className = "ds-loading-bar is-done";
    clearing = setTimeout(reset, 600);
  }
  // A page load, as opposed to one of htmx's trips. WebKit stops putting the
  // page on the screen the moment a load begins -- scripts go on running, but
  // nothing they change is shown again -- so a bar started in the same turn
  // as the load is never seen. Measured in the simulator: the page's own
  // clock stopped at the tap. So in browsers and apps built on it, the bar
  // goes up first, at once rather than after the usual moment, and the load
  // is begun a frame later, once it has been painted; what is on the screen
  // is then frozen with the bar in it, and its creep runs on the compositor,
  // which the freeze does not stop. Chromium, under the Android and Windows
  // apps, keeps painting the page until the next one arrives, so there
  // nothing is held back.
  const holds =
    /^(ios|macos)$/.test(document.documentElement.getAttribute("data-app") || "") ||
    (/AppleWebKit\//.test(navigator.userAgent) && !/(Chrome|Chromium|Edg|OPR|Android)\//.test(navigator.userAgent));
  // A page that asks before it is left -- a drawing not yet saved, a
  // collaborative session drawn in -- may be stayed on, and nothing tells the
  // page it was: the question is the browser's or the app's, and "stay"
  // simply cancels the load. A bar started at the press would then creep
  // under the header of a page going nowhere until its timeout. So the page
  // is asked first, the way the apps ask it (app_bridge.jinja), and a load it
  // would question goes without the bar.
  function wouldAsk(): boolean {
    return !!(page.oeeeApp && page.oeeeApp.wouldLoseWork && page.oeeeApp.wouldLoseWork());
  }
  function leave(go: () => void): void {
    if (wouldAsk()) return go();
    if (!holds || !window.requestAnimationFrame) {
      start(null);
      return go();
    }
    start(null, true);
    requestAnimationFrame(() => go());
  }
  // A page load the reader has just agreed to in an app's own dialog
  // (oeeeApp.leaving, app_bridge.jinja): the one load leave() cannot see
  // coming, since only the app hears the answer to the question it asked. Up
  // at once, and on WebKit a frame let pass before the promise settles, for
  // the app to begin the load after -- though never longer than a moment,
  // since a web view that is off the screen has no frames to wait for.
  function leaving(): Promise<void> {
    if (!holds || !window.requestAnimationFrame) {
      start(null);
      return Promise.resolve();
    }
    start(null, true);
    return new Promise((resolve) => {
      requestAnimationFrame(() => resolve());
      setTimeout(resolve, 100);
    });
  }
  page.oeeeLoadingBar = { start, done, leave, leaving };

  // Whether htmx will take this trip for itself. The toolbar is boosted
  // wherever it is, but the painter pages it sits above (the collaborative
  // one, served as a built file) do not load htmx, and there a boosted link is
  // an ordinary page load -- one the page may ask about first.
  function isBoosted(element: Element): boolean {
    return (
      !!page.htmx && !!element.closest('[hx-boost\\:inherited="true"]') && !element.closest('[hx-boost="false"]')
    );
  }
  // On the window, so it is heard after every listener on the document: those
  // that do something with a link the page is leaving by (a segmented
  // control's thumb sliding to it, thumbs.ts) see a click nobody has
  // cancelled yet, and what they cancel is seen here.
  window.addEventListener("click", (event) => {
    if (event.button !== 0) return;
    if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    const target = event.target as Element | null;
    const link = target && target.closest ? (target.closest("a[href]") as HTMLAnchorElement | null) : null;
    if (!link || link.target || link.hasAttribute("download")) return;
    // htmx cancels the click on a boosted link and fetches the page itself;
    // any other cancelled click is a script keeping the page.
    const boosted = isBoosted(link);
    if (event.defaultPrevented && !boosted) return;
    if (link.origin !== location.origin) return;
    if (link.pathname === location.pathname && link.search === location.search) return;
    if (boosted) return start(link);
    if (wouldAsk()) return;
    if (!holds) return start(null);
    event.preventDefault();
    const href = link.href;
    leave(() => {
      location.href = href;
    });
  });
  // A form that is sent as a page load, and a boosted one (the toolbar's)
  // that htmx sends. Forms of htmx's own -- a follow, a comment -- change the
  // page they are on and answer for themselves.
  window.addEventListener("submit", (event) => {
    const form = event.target as HTMLFormElement | null;
    if (!form || !form.getAttribute || form.target) return;
    if ((form.getAttribute("method") || "").toLowerCase() === "dialog") return;
    if (
      form.hasAttribute("hx-post") ||
      form.hasAttribute("hx-get") ||
      form.hasAttribute("hx-put") ||
      form.hasAttribute("hx-delete")
    ) {
      return;
    }
    const boosted = isBoosted(form);
    if (event.defaultPrevented && !boosted) return;
    if (boosted) return start(form);
    if (wouldAsk()) return;
    if (!holds) return start(null);
    // Sent again by hand once the bar is up. form.submit() leaves out the
    // button that was pressed, which some forms read, so it goes along as a
    // field of its own.
    event.preventDefault();
    const submitter = event.submitter as HTMLButtonElement | null;
    leave(() => {
      let carried: HTMLInputElement | null = null;
      if (submitter && submitter.name && form.contains(submitter)) {
        carried = document.createElement("input");
        carried.type = "hidden";
        carried.name = submitter.name;
        carried.value = submitter.value;
        form.appendChild(carried);
      }
      HTMLFormElement.prototype.submit.call(form);
      if (carried) {
        const added = carried;
        setTimeout(() => {
          if (added.parentNode) added.parentNode.removeChild(added);
        }, 0);
      }
    });
  });
  // Back to a page htmx has not kept: it is fetched again.
  document.addEventListener("htmx:before:history:restore", () => {
    start(document.body);
  });

  // htmx's request for the trip that started the bar is the one that ends
  // it, however it ends. The source element can be swapped away before the
  // end, so the request is remembered by its context once it is sent. htmx's
  // own click handler is on the link, so it usually sends the request before
  // the click reaches the document and starts the bar: the last request sent
  // is kept for start() to claim.
  document.addEventListener("htmx:before:request", (event) => {
    const ctx = ctxOf(event);
    if (!ctx) return;
    sent = ctx;
    if (source && ctx.sourceElement === source) request = ctx;
  });
  document.addEventListener("htmx:finally:request", (event) => {
    const ctx = ctxOf(event);
    if (!ctx || !(source || request)) return;
    if (ctx === request || ctx.sourceElement === source) done();
  });
  // A boosted swap replaces the header, and the bar with it. It is hung under
  // the new one where it had got to, and goes on from there.
  let reached: string | null = null;
  document.addEventListener("htmx:before:swap", () => {
    reached = loadingNow() && bar!.parentNode ? getComputedStyle(bar!).transform : null;
  });
  document.addEventListener("htmx:after:swap", () => {
    if (!reached || !loadingNow() || document.documentElement.contains(bar)) return;
    if (!hang()) return;
    bar!.style.transition = "none";
    bar!.style.transform = reached;
    bar!.style.opacity = "1";
    void bar!.offsetWidth;
    bar!.style.transition = "";
    bar!.style.transform = "";
    bar!.style.opacity = "";
    reached = null;
  });
  // Every other page load: a script's (the apps' menus and shortcuts, Draw's
  // form.submit(), htmx following an HX-Redirect, a retry after a gateway
  // error), back or forward to a page that was not kept, the app loading an
  // address itself. The page stays up, and running, until the next one
  // commits. Looked at once every listener has had its say: a page that asks
  // whether to leave (a drawing not yet saved) may be stayed on, and the bar
  // would hang there, so it is left without. The question put by wouldAsk()
  // above, and by the apps, is not a load.
  window.addEventListener("beforeunload", (event) => {
    if (event.isTrusted === false) return;
    setTimeout(() => {
      if (event.defaultPrevented) return;
      if (typeof event.returnValue === "string" && event.returnValue !== "") return;
      if (loadingNow()) return;
      start(null);
    }, 0);
  });
  // Back to a page kept in memory, left with the bar still going.
  window.addEventListener("pageshow", (event) => {
    if (event.persisted) reset();
  });
}
