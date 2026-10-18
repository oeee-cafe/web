// The Mac app's saved passwords (app_saved_password.jinja): the reader went
// to the username or password of a form that takes a saved one, and the app
// offers the system's saved passwords for the site in a sheet of its own.

import type { OeeeApp } from "./bridge";

type PasswordWindow = Window & typeof globalThis & { oeeeApp?: OeeeApp };

interface Told {
  id?: string;
  username?: string;
  password?: string;
}

/** Offers the sheet as a field is reached, and answers it. */
export function installSavedPassword(): void {
  const app = (window as PasswordWindow).oeeeApp;
  if (!app) return;
  // Only the Mac app asks; the answer is on every page, as every member is.
  const asking = document.documentElement.getAttribute("data-app") === "macos";

  const FIELDS = /^(username|current-password)$/;

  // The form the sheet is up for, and the id it was asked with. One at a time.
  let asked: { id: string; form: HTMLFormElement } | null = null;
  let count = 0;
  const offered = new WeakSet<HTMLFormElement>();

  // Whether the field was reached with Tab, which is the keyboard's press.
  let tabbing = false;
  document.addEventListener(
    "keydown",
    (event) => {
      tabbing = event.key === "Tab";
    },
    true,
  );

  function form(target: EventTarget | null): HTMLFormElement | null {
    const element = target as Element | null;
    const field = element && element.closest ? (element.closest("input") as HTMLInputElement | null) : null;
    if (!field || !FIELDS.test(field.getAttribute("autocomplete") || "")) return null;
    const owner = field.form;
    return owner && owner.hasAttribute("data-saved-password") ? owner : null;
  }

  function offer(target: EventTarget | null): void {
    const owner = form(target);
    if (!asking || !owner || asked || offered.has(owner)) return;
    offered.add(owner);
    const id = String(++count);
    if (app!.post("password", { id }, true)) asked = { id, form: owner };
  }

  document.addEventListener(
    "pointerdown",
    (event) => {
      tabbing = false;
      offer(event.target);
    },
    true,
  );
  document.addEventListener(
    "focusin",
    (event) => {
      if (tabbing) offer(event.target);
    },
    true,
  );

  function fill(field: HTMLInputElement | null, value: string): void {
    if (!field) return;
    field.value = value;
    field.dispatchEvent(new Event("input", { bubbles: true }));
    field.dispatchEvent(new Event("change", { bubbles: true }));
  }

  function answer(told?: Told | null): void {
    told = told || {};
    const request = asked;
    if (!request || told.id !== request.id) return;
    asked = null;
    const owner = request.form;
    // None picked or none saved: the reader types, as they would have.
    if (!owner.isConnected || typeof told.password !== "string" || !told.password) return;

    const password = owner.querySelector<HTMLInputElement>('input[autocomplete="current-password"]');
    if (owner.getAttribute("data-saved-password") !== "sign-in") {
      fill(password, told.password);
      if (password) password.focus();
      return;
    }
    fill(owner.querySelector<HTMLInputElement>('input[autocomplete="username"]'), told.username || "");
    fill(password, told.password);
    if (owner.requestSubmit) owner.requestSubmit();
    else owner.submit();
  }

  app.password = { answer };
}
