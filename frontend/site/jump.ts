// The quick switcher (jump.jinja): Ctrl+K, or ⌘K on a Mac, from anywhere on
// the site's own pages, and the `jump` command for the apps' menus
// (toolbarControls.ts). A field and one list under it -- communities, people,
// tags and pages, from /jump (web::handlers::jump, jump_results.jinja) --
// moved through with the arrow keys and left with Enter, the way a chat
// application's switcher is. The field keeps the focus throughout, and the
// list says which row is chosen with aria-activedescendant.
//
// Fetched with fetch() rather than htmx, so it works where htmx does not run
// (Firefox 56); a newer answer always wins over an older one that arrives
// after it. Going to a row is the toolbar's own `go`, with its loading bar
// and its skeleton.
//
// Everything here is the document's: the markup is found by id each time, so
// a boosted swap that brings it back needs nothing but the key's mark again.

type JumpWindow = Window &
  typeof globalThis & {
    oeeeJump?: { open(): void; close(): void };
    oeeeCommand?: (name: string) => boolean;
    oeeeGo?: (path: string) => void;
  };

const page = window as JumpWindow;

const WAIT_MS = 80;

function el<T extends HTMLElement = HTMLElement>(id: string): T | null {
  return document.getElementById(id) as T | null;
}

function closestTo(target: EventTarget | null, selector: string): Element | null {
  const element = target as Element | null;
  return element && element.closest ? element.closest(selector) : null;
}

/** Sets the switcher up, once for the document. */
export function installJump(): void {
  if (page.oeeeJump) return;
  let asked = 0;
  let pending: ReturnType<typeof setTimeout> | undefined;
  let returnTo: Element | null = null;

  function rows(): NodeListOf<HTMLElement> | [] {
    const list = el("jump-list");
    return list ? list.querySelectorAll<HTMLElement>(".jump-item") : [];
  }
  function chosen(): Element | null {
    const list = el("jump-list");
    return list ? list.querySelector('.jump-item[aria-selected="true"]') : null;
  }
  function choose(index: number): void {
    const all = rows();
    const field = el("jump-field");
    if (!all.length) {
      if (field) field.removeAttribute("aria-activedescendant");
      return;
    }
    index = (index + all.length) % all.length;
    for (let i = 0; i < all.length; i++) {
      all[i].id = "jump-row-" + i;
      all[i].setAttribute("aria-selected", i === index ? "true" : "false");
    }
    if (field) field.setAttribute("aria-activedescendant", all[index].id);
    if ("scrollIntoView" in all[index]) all[index].scrollIntoView({ block: "nearest" });
  }
  function indexOf(row: Element | null): number {
    const all = rows();
    for (let i = 0; i < all.length; i++) if (all[i] === row) return i;
    return -1;
  }
  function load(q: string): void {
    const mine = ++asked;
    fetch("/jump?q=" + encodeURIComponent(q), { credentials: "same-origin" })
      .then((response) => {
        if (!response.ok) throw new Error(String(response.status));
        return response.text();
      })
      .then((html) => {
        const list = el("jump-list");
        if (mine !== asked || !list) return;
        list.innerHTML = html;
        choose(0);
      })
      .catch(() => {});
  }
  function open(): void {
    const jump = el("jump");
    const field = el<HTMLInputElement>("jump-field");
    if (!jump || !field) return;
    if (!jump.hidden) {
      field.select();
      return;
    }
    returnTo = document.activeElement;
    jump.hidden = false;
    field.value = "";
    load("");
    field.focus();
  }
  function close(): void {
    const jump = el("jump");
    if (!jump || jump.hidden) return;
    jump.hidden = true;
    const back = returnTo as HTMLElement | null;
    if (back && back.focus && document.contains(back)) back.focus();
    returnTo = null;
  }
  function follow(row: Element | null, event?: KeyboardEvent | MouseEvent): void {
    if (!row) return;
    const inTab = event && (event.metaKey || event.ctrlKey || event.shiftKey);
    const href = row.getAttribute("href")!;
    if (inTab) {
      window.open(href, "_blank");
      return;
    }
    close();
    const name = row.getAttribute("data-command");
    if (name && page.oeeeCommand && page.oeeeCommand(name)) return;
    if (page.oeeeGo) page.oeeeGo(href);
    else location.href = href;
  }
  page.oeeeJump = { open, close };

  // The key that opens it sits at the field's end, so that someone who came
  // by the toolbar's search button learns the way that skips it: Ctrl K, or
  // ⌘ K where the platform says it is a Mac or an iPad.
  function markKey(): void {
    const mod = el("jump-key-mod");
    if (mod && /Mac|iPhone|iPad|iPod/.test(navigator.platform || "")) mod.textContent = "⌘";
  }
  markKey();
  // A boosted swap brings the markup back as the server wrote it.
  document.addEventListener("htmx:after:swap", markKey);

  document.addEventListener("keydown", (event) => {
    const key = event.key && event.key.toLowerCase();
    if (key === "k" && (event.ctrlKey || event.metaKey) && !event.altKey && !event.shiftKey) {
      if (!document.body || !document.body.classList.contains("ds-page")) return;
      event.preventDefault();
      const jump = el("jump");
      if (jump && !jump.hidden) close();
      else open();
      return;
    }
    const target = event.target as Element | null;
    if (!target || target.id !== "jump-field") return;
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      choose(indexOf(chosen()) + (event.key === "ArrowDown" ? 1 : -1));
    } else if (event.key === "Enter") {
      event.preventDefault();
      follow(chosen(), event);
    } else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      close();
    } else if (event.key === "Tab") {
      // The field is the dialog's one stop; the list is chosen from it.
      event.preventDefault();
      choose(indexOf(chosen()) + (event.shiftKey ? -1 : 1));
    }
  });
  document.addEventListener("input", (event) => {
    const target = event.target as HTMLInputElement | null;
    if (!target || target.id !== "jump-field") return;
    clearTimeout(pending);
    const q = target.value;
    pending = setTimeout(() => load(q), WAIT_MS);
  });
  document.addEventListener("mousemove", (event) => {
    const row = closestTo(event.target, "#jump-list .jump-item");
    if (row && row.getAttribute("aria-selected") !== "true") choose(indexOf(row));
  });
  document.addEventListener("click", (event) => {
    const target = event.target as Element | null;
    if (!target || !target.closest) return;
    if (target.id === "jump") {
      close();
      return;
    }
    const row = target.closest("#jump-list .jump-item");
    if (!row || event.button !== 0) return;
    event.preventDefault();
    follow(row, event);
  });
}
