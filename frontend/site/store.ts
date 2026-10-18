// The Supporter Pack, bought through the store the app's build sells through
// (app_store.jinja): what the store sold, handed to the site, and the ticket
// the Microsoft Store wants first.

import type { OeeeApp } from "./bridge";

type StoreWindow = Window & typeof globalThis & { oeeeApp?: OeeeApp };

interface Store {
  ended?: (outcome: unknown) => void;
  purchased?: (proofs: unknown) => Promise<string[]>;
  ticket?: () => Promise<{ ticket: string; user: string } | null>;
  [member: string]: unknown;
}

function tell(detail: Record<string, unknown>): void {
  let event: CustomEvent;
  try {
    event = new CustomEvent("oeee:store", { detail });
  } catch {
    return;
  }
  document.dispatchEvent(event);
}

/** Adds what the store answers with to oeeeApp.store. */
export function installStore(): void {
  const app = (window as StoreWindow).oeeeApp;
  if (!app) return;

  const store: Store = (app.store as Store) || {};
  app.store = store;
  store.ended = (outcome) => {
    tell({ state: "ended", outcome: String(outcome) });
  };
  store.purchased = (proofs) => {
    const sells = document.documentElement.getAttribute("data-store");
    let list = Array.isArray(proofs)
      ? (proofs as unknown[]).filter((proof): proof is string => typeof proof === "string" && !!proof)
      : [];
    if (!sells) list = [];
    if (list.length) tell({ state: "checking" });
    return Promise.all(
      list.map((proof) =>
        fetch("/store/" + sells + "/purchases", {
          method: "POST",
          credentials: "same-origin",
          headers: { "Content-Type": "application/x-www-form-urlencoded" },
          body: "proof=" + encodeURIComponent(proof),
        }).then(
          // A plain yes only. Nobody signed in (401), too many at once (429)
          // and the store out of reach (502) are all worth offering again, so
          // none of them lets the app finish.
          (response) => ({ proof, status: response.status }),
          // Never reached the site: as good as a store out of reach.
          () => ({ proof, status: 0 }),
        ),
      ),
    ).then((answers) => {
      const taken: string[] = [];
      const refused: number[] = [];
      answers.forEach((answer) => {
        if (answer.status === 204) taken.push(answer.proof);
        else refused.push(answer.status);
      });
      if (list.length) tell({ state: "checked", taken: taken.length, refused });
      // After the answer has gone back, so the app hears it before the page
      // it came from goes away. Only /supporter shows the pack.
      if (taken.length && location.pathname === "/supporter") {
        setTimeout(() => location.reload(), 0);
      }
      return taken;
    });
  };
  store.ticket = () => {
    if (document.documentElement.getAttribute("data-store") !== "microsoft") {
      return Promise.resolve(null);
    }
    return fetch("/store/microsoft/tickets", {
      method: "POST",
      credentials: "same-origin",
    })
      .then((response) => (response.status === 200 ? response.json() : null))
      .then(
        (told: { ticket?: unknown; user?: unknown } | null) =>
          // Exactly the two strings the Store is to be given, or nothing.
          told && typeof told.ticket === "string" && told.ticket && typeof told.user === "string" && told.user
            ? { ticket: told.ticket, user: told.user }
            : null,
        () => null,
      );
  };
}
