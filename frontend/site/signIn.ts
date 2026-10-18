// Signing in inside an app (app_sign_in.jinja holds the why): a sheet of the
// platform's own where it has one, a ticket from Steam in the Steam build,
// and otherwise the system's browser, handed back to the page by a handoff
// the page keeps asking after. Called once a document from the head, with
// the words it may have to say.

import type { OeeeApp } from "./bridge";

interface Started {
  state?: string;
  nonce?: string;
  id?: string;
  secret?: string;
  url?: string;
  code?: string;
}

interface Told {
  cancelled?: boolean;
  ticket?: string;
  id_token?: string;
  user?: string;
}

type SignInWindow = Window &
  typeof globalThis & {
    oeeeApp?: OeeeApp;
    oeeeLoadingBar?: { start(from?: Element | null, now?: boolean): void; done(): void };
    dsAlert?: (text: string) => void;
    dsConfirm?: (text: string, action?: string, tone?: string) => Promise<boolean>;
  };

const page = window as SignInWindow;

type Way = (provider: string, next: string | null) => void;

/** Takes the sign-in buttons' presses in an app, and answers it. */
export function installSignIn(STEAM_FAILED: string, OK: string): void {
  if (!page.oeeeApp) return;
  const app = page.oeeeApp;

  function form(fields: Record<string, string | null | undefined>): string {
    const body = new URLSearchParams();
    for (const name in fields) {
      if (fields[name]) body.set(name, fields[name]!);
    }
    return body.toString();
  }

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function post(path: string, fields: Record<string, string | null | undefined>): Promise<any> {
    return fetch(path, {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body: form(fields),
    }).then((response) => (response.ok ? response.json() : null));
  }

  // Replaced, not followed: the page signed in from is not left in the
  // history behind the one it lands on, so Back does not return to a sign-in
  // form for an account already signed in. The site's notice is waiting in
  // the session and is shown by the page this replaces with.
  function go(next?: string): void {
    location.replace(next || "/");
  }

  // The loading bar (loadingBar.ts), from the moment a sign-in comes back
  // with something until the page it lands on replaces this one. Between the
  // two the site is checking what came back and nothing on this page
  // changes, so without it the page looked stuck. Up at once: the reader has
  // just come back to the page and is looking at it.
  function working(): void {
    if (page.oeeeLoadingBar) page.oeeeLoadingBar.start(null, true);
  }
  function idle(): void {
    if (page.oeeeLoadingBar) page.oeeeLoadingBar.done();
  }

  // -- A sheet of the platform's own -------------------------------------

  // The sign-in under way: its provider and state. One at a time.
  let asked: { provider: string; state: string | null } | null = null;

  function native(provider: string, next: string | null): void {
    if (asked) return;
    asked = { provider, state: null };
    post("/auth/" + provider + "/start", { next })
      .then((started: Started | null) => {
        if (!started || !started.state || !started.nonce) {
          asked = null;
          return;
        }
        // Whatever is asked now: answered and let go meanwhile, this throws,
        // and the catch below lets go of the start too.
        asked!.state = started.state;
        if (!app.post("signIn", { provider, nonce: started.nonce }, true)) {
          asked = null;
        }
      })
      .catch(() => {
        asked = null;
      });
  }

  // -- Steam --------------------------------------------------------------

  // Only the build that sells through Steam can ask Steam for a ticket
  // (data-store="steam"), and its button is a link to /auth/steam, which a
  // browser follows to Steam's own sign-in page. No nonce: the ticket names
  // the site's own identity ("oeee-cafe", steam/client.rs in
  // oeee-cafe-desktop) and the site asks Steam about it (src/steam.rs).
  let steamNext: string | null = null;
  let askedSteam = false;

  function steam(_provider: string, next: string | null): void {
    if (askedSteam) return;
    askedSteam = app.post("signIn", { provider: "steam" }, true);
    steamNext = next;
  }

  // The ticket is posted as a form, so the site's answer is a page and its
  // notices come with it, as they would from any sign-in.
  function steamAnswered(told: Told): void {
    askedSteam = false;
    if (told.cancelled) return;
    if (!told.ticket) {
      page.dsAlert!(STEAM_FAILED);
      return;
    }
    working();
    const sending = document.createElement("form");
    sending.method = "post";
    sending.action = "/auth/steam";
    const fields: Record<string, string | null> = { ticket: told.ticket, next: steamNext };
    for (const name in fields) {
      if (!fields[name]) continue;
      const input = document.createElement("input");
      input.type = "hidden";
      input.name = name;
      input.value = fields[name]!;
      sending.appendChild(input);
    }
    document.body.appendChild(sending);
    sending.submit();
  }

  function answer(told?: Told | null): void {
    told = told || {};
    if (askedSteam) {
      steamAnswered(told);
      return;
    }
    const request = asked;
    asked = null;
    if (!request || !request.state) return;
    // Put away without signing in: nothing went wrong, so nothing is said.
    if (told.cancelled) return;
    // Without a token the site is still asked, with an error, so that it says
    // it could not confirm who this is in the reader's own language.
    const fields = told.id_token
      ? { state: request.state, id_token: told.id_token, user: told.user, format: "json" }
      : { state: request.state, error: "failed", format: "json" };
    working();
    post("/auth/" + request.provider, fields)
      .then((where: { next?: string } | null) => {
        if (where) go(where.next);
        else idle();
      })
      .catch(idle);
  }

  // -- The system's browser, and back ------------------------------------

  // The handoff under way: its id, its secret, and its URL -- and, once the
  // app has been handed it, the browser's code.
  let pending: Started | null = null;
  const CODE = /^[0-9a-f]{64}$/;
  let asking: ReturnType<typeof setInterval> | null = null;
  // Whether an ask is in flight. Two at once can spend the handoff between
  // them: one gets the sign-in and the other "there is no such handoff", and
  // whichever lands second decides what the page does -- which once left a
  // successful sign-in on the floor, because the failure arrived first and
  // cleared `pending` out from under it.
  let inFlight = false;
  // Whether the reader is being asked something, which the asking waits on.
  let deciding = false;
  // Whether the app has said the browser is gone (unopened), so the next
  // answer that is not the sign-in is the last one asked for.
  let closing = false;

  // How often to ask whether the browser has finished, and for how long. The
  // site forgets a handoff after fifteen minutes (src/handoff.rs), so there
  // is nothing to find after that.
  const ASK_EVERY = 2000;
  const GIVE_UP_AFTER = 15 * 60 * 1000;

  // A yes or no from the reader, in the site's own dialog (alerts.ts, on
  // every page this is), as a plain question rather than a destructive one.
  function agree(message: string): Promise<boolean> {
    return page.dsConfirm!(message, OK, "plain");
  }

  function stop(): void {
    if (asking) {
      clearInterval(asking);
      asking = null;
    }
    if (pending && pending.code) idle();
    pending = null;
    closing = false;
  }

  function ask(confirmed: boolean): void {
    if (!pending || inFlight || deciding) return;
    inFlight = true;
    const fields: Record<string, string | undefined> = { id: pending.id, secret: pending.secret, code: pending.code };
    if (confirmed) fields.confirm = "1";
    post("/auth/handoff/claim", fields)
      .then((claimed: { status?: string; message?: string; next?: string } | null) => {
        inFlight = false;
        // No answer decides nothing, and neither does "failed": the handoff
        // is still good, so keep asking rather than give up on a sign-in that
        // may well have taken.
        if (!pending) return;
        if (!claimed || claimed.status === "waiting" || claimed.status === "failed") {
          if (closing) stop();
          return;
        }
        if (claimed.status === "confirm") {
          // Linking the browser's account to the one signed in here. The site
          // words the question, in the reader's language, and asks it in its
          // own dialog, which unlike the browser's does not stop the page:
          // nothing is asked of the site again until it is answered.
          deciding = true;
          idle();
          agree(claimed.message!).then((yes) => {
            deciding = false;
            if (yes) {
              working();
              ask(true);
            } else {
              stop();
            }
          });
          return;
        }
        if (claimed.status === "ready") {
          const next = claimed.next;
          if (asking) clearInterval(asking);
          asking = null;
          pending = null;
          go(next);
          return;
        }
        // "unknown", or anything a later site says that this does not know:
        // the sign-in is not going to arrive.
        stop();
      })
      .catch(() => {
        // A request that did not land says nothing either way; the next will
        // -- unless there is to be no next.
        inFlight = false;
        if (closing && pending) stop();
      });
  }

  function browser(provider: string, next: string | null, at?: string): void {
    if (pending) return;
    post("/auth/handoff/start", { provider, next, at })
      .then((started: Started | null) => {
        if (!started || !started.id || !started.secret || !started.url) return;
        pending = started;
        if (!app.post("browse", { url: new URL(started.url, document.baseURI).href }, true)) {
          stop();
          return;
        }
        asking = setInterval(() => ask(false), ASK_EVERY);
        setTimeout(() => {
          if (pending === started) stop();
        }, GIVE_UP_AFTER);
      })
      .catch(() => {});
  }

  // -- Which way, and when -----------------------------------------------

  // How each app signs in with each provider: a sheet of its own where the
  // platform has one, the system's browser where it does not. The app is the
  // one the root is marked for (theme_head.jinja). A browser sign-in with
  // Google goes straight to Google's page, which is what the reader pressed
  // for, rather than through this site's (handoff_start, `at`). The browser
  // the Apple apps open (ASWebAuthenticationSession) first asks whether the
  // app may use the first page's domain to sign in, which then names Google;
  // the Windows app's browser asks nothing, and is saved the stop here.
  function atProvider(provider: string, next: string | null): void {
    browser(provider, next, "provider");
  }

  // Discord, in a browser in every app: the person's Discord password or
  // passkey is in the browser, not here. The Apple apps go straight to
  // Discord's page, for the same question Google's is sent there for; the
  // others by this site's own. Steam by this site's, in every app but the
  // Steam build, which asks Steam for a ticket instead (above).
  const WAYS: Record<string, Record<string, Way>> = {
    ios: { apple: native, google: atProvider, discord: atProvider, steam: browser },
    macos: { apple: native, google: atProvider, discord: atProvider, steam: browser },
    android: { apple: browser, google: native, discord: browser, steam: browser },
    windows: { apple: browser, google: atProvider, discord: browser, steam: browser },
  };
  const root = document.documentElement;
  const appName = root.getAttribute("data-app") || "";
  let ways: Record<string, Way> | undefined = Object.prototype.hasOwnProperty.call(WAYS, appName)
    ? WAYS[appName]
    : undefined;
  if (root.getAttribute("data-store") === "steam") {
    ways = ways || {};
    ways.steam = steam;
  }

  // The sign-in buttons (login.jinja, account.jinja) are links to the
  // provider's page, which is where a browser goes. In an app the page takes
  // the press itself and goes one of the ways above; the app never has to
  // recognise the link, and the toolbar lays no skeleton for it.
  if (ways) {
    const chosen = ways;
    document.addEventListener(
      "click",
      (event) => {
        if (event.defaultPrevented || event.button !== 0) return;
        const target = event.target as Element | null;
        const link = target && target.closest ? (target.closest("a[href]") as HTMLAnchorElement | null) : null;
        if (!link || link.origin !== new URL(document.baseURI).origin) return;
        const provider = (/^\/auth\/(apple|google|discord|steam)$/.exec(link.pathname) || [])[1];
        if (!provider || !chosen[provider]) return;
        event.preventDefault();
        const next = new URL(link.href).searchParams.get("next");
        chosen[provider](provider, next);
      },
      true,
    );
  }

  app.signIn = {
    answer,
    // Back in front, probably from the browser: ask now rather than at the
    // next turn of the clock. `code` is the one in
    // oeee-cafe://handoff/done?code=..., when that is how the app came back;
    // it is kept for every ask after, since nothing is handed over without
    // it. Anything that is not a code is no code. A code is the browser
    // having finished, so from here the page is on its way and shows it;
    // brought back without one, the reader may only have looked away, and
    // nothing is shown.
    resume: (code?: unknown) => {
      if (!pending) return;
      if (typeof code === "string" && CODE.test(code)) {
        pending.code = code;
        working();
      }
      ask(false);
    },
    // The browser could not be opened, or was put away. Put away can be after
    // the sign-in finished there and before the page asked, so the site is
    // asked once more, and the handoff is let go only if that answer is not
    // the sign-in -- an ask already under way is that one.
    unopened: () => {
      if (!pending) return;
      closing = true;
      if (!inFlight) ask(false);
    },
  };
}
