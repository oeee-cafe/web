// /supporter (supporter.jinja). Buying happens in the app: the page asks, the
// app sells, and the app hands what the store gave it back to the page, which
// tells the site (store.ts) and reloads. Nothing here records anything -- a
// page cannot be trusted about a purchase, and is not; the site asks the
// store itself.
//
// The price is the store's to say, in the reader's own currency and formatted
// its way, so the button leaves room for it and the app fills it in
// (bridge.ts). A button with no price is a button whose app has not
// answered; it still buys the pack, and the sheet the store opens says the
// price either way.

import type { OeeeApp } from "../bridge";

/** What the page says of a press, in the reader's language. */
export interface SupporterWords {
  opening: string;
  restoring: string;
  checking: string;
  restored: string;
  nothing: string;
  pending: string;
  failed: string;
  refused: string;
  later: string;
  signIn: string;
}

type SupporterWindow = Window & typeof globalThis & { oeeeApp?: OeeeApp };

export function supporterPage(WORDS: SupporterWords): void {
  function ask(type: string, data: Record<string, unknown>): boolean {
    const app = (window as SupporterWindow).oeeeApp;
    if (!app || !app.post) return false;
    return app.post(type, data, true);
  }

  // Defined before the asking, so an app that answers at once is heard.
  // Called again whenever the app knows better -- a currency that arrived
  // late, a price that changed while the page was open.
  const app = (window as SupporterWindow).oeeeApp;
  if (app) {
    const store = (app.store as Record<string, unknown>) || {};
    app.store = store;
    store.prices = (prices: Record<string, unknown> | null) => {
      if (!prices) return;
      const spans = document.querySelectorAll(".supporter-price");
      for (let i = 0; i < spans.length; i++) {
        const price = prices[spans[i].getAttribute("data-product")!];
        spans[i].textContent = price == null ? "" : String(price);
      }
    };
  }

  // Every button here is the store's this build sells through -- the server
  // drew no other -- so nothing the app is asked needs to say which.
  const buttons = document.querySelectorAll(".supporter-buy");
  if (buttons.length) {
    const products: (string | null)[] = [];
    for (let i = 0; i < buttons.length; i++) products.push(buttons[i].getAttribute("data-product"));
    ask("prices", { products });
  }

  // What became of a press, said under the buttons. A pack bought is said by
  // the page the site reloads to, which has it; a Restore might give back
  // nothing that page shows differently, so it leaves a note for the page
  // after it to say.
  const NOTE = "oeee-supporter-note";
  const status = document.querySelector<HTMLElement>(".supporter-status");
  // Said with the spinner while something is still to come, and without it
  // once the press has its answer.
  function say(words: string, error?: boolean, working?: boolean): void {
    if (!status) return;
    status.querySelector(".supporter-status-words")!.textContent = words || "";
    status.querySelector<HTMLElement>(".ds-spinner")!.hidden = !working;
    status.hidden = !words;
    status.classList.toggle("supporter-status-error", !!error);
  }
  // Every button waits while one press is out, so a second press cannot open
  // a second sheet over the first. They all look it; the one pressed is
  // marked busy for a screen reader, and stays so until they all let go.
  function busy(on: boolean, button?: Element): void {
    const controls = document.querySelectorAll<HTMLButtonElement>(".supporter-buy, .supporter-restore");
    for (let i = 0; i < controls.length; i++) {
      controls[i].disabled = on;
      if (on && controls[i] === button) controls[i].setAttribute("aria-busy", "true");
      else if (!on || button) controls[i].removeAttribute("aria-busy");
    }
  }
  try {
    if (sessionStorage.getItem(NOTE) === "restored") say(WORDS.restored);
    sessionStorage.removeItem(NOTE);
  } catch {
    // Storage refused: the page after a Restore says nothing of it.
  }

  // What was last pressed, while its answer is outstanding. Proof the app
  // hands over unasked -- a purchase it found unfinished as the page opened --
  // is checked quietly, and is shown by the reload when the site takes it.
  //
  // The apps answer every press, with proof or with ended() -- all but
  // Steam's, whose overlay cannot tell a press given up from a purchase still
  // on its way (oeee-cafe-desktop, steam.rs). So a press lets the buttons go
  // again on its own: at once, in effect, on Steam, where the overlay is the
  // whole of what happens next; and elsewhere only long after any store
  // should have answered, so that an app that never does cannot leave the
  // page stuck. Either way the press stays outstanding, and an answer that
  // comes after is still said.
  let pressed: string | null = null;
  let waiting: ReturnType<typeof setTimeout> | undefined;
  function press(kind: string, button: Element, words: string): void {
    pressed = kind;
    busy(true, button);
    say(words, false, true);
    clearTimeout(waiting);
    const steam = document.documentElement.getAttribute("data-store") === "steam";
    waiting = setTimeout(
      () => {
        busy(false);
        say("");
      },
      steam ? 4000 : 120000,
    );
  }
  document.addEventListener("oeee:store", (event) => {
    const told = ((event as CustomEvent).detail || {}) as {
      state?: string;
      taken?: number;
      refused?: number[];
      outcome?: string;
    };
    if (!pressed) return;
    clearTimeout(waiting);
    if (told.state === "checking") {
      busy(true);
      say(WORDS.checking, false, true);
      return;
    }
    const was = pressed;
    pressed = null;
    if (told.state === "checked" && told.taken) {
      if (was === "restore") {
        try {
          sessionStorage.setItem(NOTE, "restored");
        } catch {
          // Nothing to leave the note in; the reload says it alone.
        }
      }
      // The site reloads the page to show the pack (store.ts), and until it
      // does this is still working.
      busy(true);
      say(WORDS.checking, false, true);
      return;
    }
    busy(false);
    if (told.state === "checked") {
      const refused = told.refused || [];
      if (refused.indexOf(401) !== -1) say(WORDS.signIn, true);
      // Proof the store does not know, or a site not set up to ask it:
      // offering it again would be answered the same way.
      else if (refused.every((code) => code === 400 || code === 404)) say(WORDS.refused, true);
      else say(WORDS.later, true);
      return;
    }
    if (told.state === "ended") {
      if (told.outcome === "pending") say(WORDS.pending);
      else if (told.outcome === "nothing") say(WORDS.nothing);
      else if (told.outcome === "failed") say(WORDS.failed, true);
      else say("");
    }
  });

  document.addEventListener("click", (event) => {
    const target = event.target as Element;
    const buy = target.closest && (target.closest(".supporter-buy") as HTMLButtonElement | null);
    if (buy) {
      if (buy.disabled) return;
      say("");
      if (ask("purchase", { product: buy.getAttribute("data-product") })) press("purchase", buy, WORDS.opening);
      return;
    }
    const restore = target.closest && (target.closest(".supporter-restore") as HTMLButtonElement | null);
    if (restore && !restore.disabled) {
      say("");
      if (ask("restore", {})) press("restore", restore, WORDS.restoring);
    }
  });
}
