// What an open page hears without asking (live.jinja; crate::live,
// web::handlers::events).
//
// One EventSource for the life of the window: htmx's boosted switches keep
// the window, and so keep it, and the browser reconnects it by itself --
// across a deploy, to whichever colour serves by then.
//
// unread        The bell, rendered for this reader: put in place of the one
//               in the toolbar, which the apps' icon badges follow
//               (app_bridge.jinja, on `oeee:bell`).
// notification  A new notification's words, as a toast linking to it --
//               everywhere, the apps included, beside the system's own
//               notification for it. The phone apps and the Mac's are sent
//               that as a push; Windows is sent no push, so the page hands
//               the words to the app (`notify`, app_bridge.jinja), which
//               shows them as the system's notification.
// comments      A post's comments changed: a page showing them
//               (`#comments[data-live-post]`, post_view.jinja) fetches them
//               again and morphs them in, the form beside them untouched.
//               The page that made the change already has them.
// post          A drawing was published: a page with a feed that would show
//               it (`[data-live-posts]`, "recent" or "community:<id>") puts
//               up a pill that brings the feed up to date.
//
// None of it runs where htmx does not (Firefox 56) beyond the bell and the
// toasts, which are plain DOM.

interface Htmx {
  process?: (element: Element) => void;
  ajax(verb: string, path: string, options: Record<string, unknown>): Promise<unknown>;
}

type LiveWindow = Window &
  typeof globalThis & {
    htmx?: Htmx;
    oeeeApp?: { post(type: string, data: Record<string, unknown>, now?: boolean): unknown };
    oeeeToasts?: { look(): void };
    oeeeLive?: EventSource;
  };

const page = window as LiveWindow;

let started = false;

/**
 * Starts listening, once for the window. `closeLabel` and `newDrawings` are
 * the page's words for a toast's close button and the new-drawings pill.
 */
export function startLive(closeLabel: string, newDrawings: string): void {
  if (started || !window.EventSource) return;
  started = true;
  const root = document.documentElement;
  // What each event does, by name; attached to every stream opened below.
  const heard: Record<string, (event: MessageEvent) => void> = {};
  function on(name: string, listener: (event: MessageEvent) => void): void {
    heard[name] = listener;
  }

  function me(): string | null {
    const bar = document.querySelector(".nav-bar[data-user-id]");
    return bar ? bar.getAttribute("data-user-id") : null;
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function read(event: MessageEvent): any {
    try {
      return JSON.parse(event.data);
    } catch {
      return null;
    }
  }

  on("unread", (event) => {
    const data = read(event);
    const bell = document.getElementById("nav-notifications");
    if (!data || !bell) return;
    const holder = document.createElement("div");
    holder.innerHTML = data.html;
    const next = holder.firstElementChild;
    if (!next) return;
    bell.parentNode!.replaceChild(next, bell);
    if (page.htmx && page.htmx.process) page.htmx.process(next);
    document.dispatchEvent(new CustomEvent("oeee:bell"));
  });

  on("notification", (event) => {
    const data = read(event);
    if (!data) return;
    if (root.getAttribute("data-app") === "windows" && page.oeeeApp) {
      page.oeeeApp.post("notify", { title: data.title, body: data.body, url: data.url }, true);
    }
    const stack = document.getElementById("toasts");
    if (!stack) return;
    const toast = document.createElement("div");
    toast.className = "ds-notice ds-toast ds-toast-notification";
    const body = document.createElement("a");
    body.className = "ds-notice-body";
    body.href = data.url;
    // One sentence. A push has a title and a body, and most bodies say it all
    // -- "oeee reacted with ❤️" under "oeee reacted to your post" -- so the
    // title would be said twice. Where the body quotes what someone wrote (a
    // comment, a guestbook entry), the title is what says who, and the quote
    // follows it.
    if (data.quoted && data.body) {
      const title = document.createElement("strong");
      title.textContent = data.title;
      body.appendChild(title);
      body.appendChild(document.createTextNode(" “" + data.body + "”"));
    } else {
      body.textContent = data.body || data.title;
    }
    toast.appendChild(body);
    const close = document.createElement("button");
    close.type = "button";
    close.className = "ds-notice-close";
    close.setAttribute("aria-label", closeLabel);
    close.title = closeLabel;
    close.innerHTML =
      '<svg viewBox="0 0 12 12" aria-hidden="true"><path d="M2.5 2.5l7 7m0-7l-7 7" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" /></svg>';
    close.addEventListener("click", () => {
      if (toast.parentNode) toast.parentNode.removeChild(toast);
    });
    toast.appendChild(close);
    stack.appendChild(toast);
    if (page.oeeeToasts) page.oeeeToasts.look();
  });

  on("comments", (event) => {
    const data = read(event);
    if (!data || !page.htmx) return;
    const list = document.querySelector('#comments[data-live-post="' + data.post_id + '"]');
    if (!list || (data.by && data.by === me())) return;
    page.htmx
      .ajax("GET", location.pathname + location.search, {
        target: "#comments",
        select: "#comments",
        swap: "outerMorph",
      })
      .catch(() => {});
  });

  on("post", (event) => {
    const data = read(event);
    if (!data || !page.htmx || (data.by && data.by === me())) return;
    const feeds = document.querySelectorAll("[data-live-posts]");
    for (let i = 0; i < feeds.length; i++) {
      const keys = " " + feeds[i].getAttribute("data-live-posts") + " ";
      const wants =
        (data.recent && keys.indexOf(" recent ") >= 0) ||
        (data.community_id && keys.indexOf(" community:" + data.community_id + " ") >= 0);
      if (wants) {
        announce();
        return;
      }
    }
  });

  // One pill for the page, floating just under the toolbar so it is seen
  // wherever the reader has scrolled to, the way a chat's "new messages" bar
  // is. It lives outside the content area, so any switch of that area takes
  // it down: whatever is shown then is newer than what it counted.
  let pill: HTMLButtonElement | null = null;
  function takeDown(): void {
    if (pill && pill.parentNode) pill.parentNode.removeChild(pill);
    pill = null;
  }
  document.addEventListener("htmx:after:swap", (event) => {
    const detail = (event as CustomEvent<{ ctx?: { target?: Element } }>).detail;
    const target = detail && detail.ctx && detail.ctx.target;
    if (target && (target === document.body || (target.matches && target.matches("main.ds-content")))) takeDown();
  });
  function announce(): void {
    if (!pill) {
      const made = document.createElement("button");
      pill = made;
      made.type = "button";
      made.className = "live-pill";
      made.setAttribute("data-count", "0");
      const header = document.querySelector(".ds-page > header");
      made.style.top = (header ? header.getBoundingClientRect().bottom : 0) + 8 + "px";
      made.addEventListener("click", () => {
        made.disabled = true;
        page.htmx!.ajax("GET", location.pathname + location.search, {
          target: "main.ds-content",
          select: "main.ds-content",
          swap: "outerMorph",
        }).then(
          () => {
            takeDown();
            const content = document.querySelector("main.ds-content");
            if (content && content.scrollTo) content.scrollTo(0, 0);
            window.scrollTo(0, 0);
          },
          () => {
            if (pill) pill.disabled = false;
          },
        );
      });
      document.body.appendChild(made);
    }
    const count = (parseInt(pill.getAttribute("data-count")!, 10) || 0) + 1;
    pill.setAttribute("data-count", String(count));
    pill.textContent = newDrawings + " · " + count;
  }

  // The stream. A network that drops is reconnected by the browser; an answer
  // that is not 200 -- a 502 or 530 while the tunnel or a deploy blinks --
  // closes it for good, and the page went on deaf until it was loaded again.
  // So a closed stream is opened again here: soon, then less often, and at
  // once when the page is looked at again or the network comes back.
  let source: EventSource | null = null;
  let wait = 2000;
  let retry: ReturnType<typeof setTimeout> | null = null;
  function open(): void {
    if (retry) clearTimeout(retry);
    retry = null;
    if (source && source.readyState !== 2) return;
    const opened = new EventSource("/events");
    source = opened;
    page.oeeeLive = opened;
    for (const name in heard) opened.addEventListener(name, heard[name] as EventListener);
    opened.addEventListener("open", () => {
      wait = 2000;
    });
    opened.addEventListener("error", () => {
      if (source!.readyState !== 2 || retry) return;
      retry = setTimeout(open, wait);
      wait = Math.min(wait * 2, 60000);
    });
  }
  function openIfClosed(): void {
    if (!source || source.readyState === 2) open();
  }
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden) openIfClosed();
  });
  window.addEventListener("online", openIfClosed);
  open();
}
