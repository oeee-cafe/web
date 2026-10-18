// The raised choice in a segmented control (segmented.jinja) -- every
// .ds-segmented, the toolbar's sections among them -- slides to the next
// rather than jumping. It is a thumb the group draws behind its items
// (ds.css), put under whichever item is chosen: aria-current="page" for a
// group of links, aria-pressed="true" for a group of buttons, or
// .is-selected.
//
// It moves when the choice does, whoever changes it: a page's own script
// pressing a tab, the toolbar marking the section being gone to
// (oeeeMarkCurrent), or a press here on a link in a page's pill, which is
// marked chosen at once and switched to in place, the pill left as it is
// (pill_macro.jinja). Anything else that moves the items -- a page arriving,
// a font loading, the window resizing, a menu opening on a group that was
// hidden -- puts it in place without sliding.
//
// The boost swaps the body and runs segmented.jinja's line again, which
// places the new page's groups; the document's listeners are added once.

type ThumbsWindow = Window & typeof globalThis & { oeeeSettleThumbs?: () => void };

/** A group, with what a press on its pill chose and what it replaced. */
type Group = HTMLElement & { oeeeWas?: Element | null; oeeeNow?: Element | null };

const GROUPS = ".ds-segmented";

function isGroup(node: Node | null): node is Group {
  return !!node && node.nodeType === 1 && !!(node as Element).matches && (node as Element).matches(GROUPS);
}

function chosen(group: Element): HTMLElement | null {
  for (let i = 0; i < group.children.length; i++) {
    const item = group.children[i] as HTMLElement;
    if (
      item.getAttribute("aria-current") === "page" ||
      item.getAttribute("aria-pressed") === "true" ||
      item.classList.contains("is-selected")
    ) {
      return item;
    }
  }
  return null;
}

function place(group: HTMLElement, moving: boolean): void {
  const item = chosen(group);
  // Nothing chosen, or not laid out (a closed menu): the item's own raised
  // look stands in until it can be measured.
  if (!item || !item.offsetWidth) {
    group.classList.remove("has-thumb", "thumb-moves");
    return;
  }
  if (moving && group.classList.contains("has-thumb")) {
    // Where the thumb is now, settled before it is told where to go, or a
    // group just moved in the document would have no start.
    void group.offsetWidth;
    group.classList.add("thumb-moves");
  } else {
    group.classList.remove("thumb-moves");
  }
  // Marked before it is measured: the choice is drawn at its neighbours'
  // weight once the thumb is under it (ds.css), and the thumb is to fit that.
  group.classList.add("has-thumb");
  group.style.setProperty("--thumb-x", item.offsetLeft + "px");
  group.style.setProperty("--thumb-y", item.offsetTop + "px");
  group.style.setProperty("--thumb-w", item.offsetWidth + "px");
  group.style.setProperty("--thumb-h", item.offsetHeight + "px");
}

function settle(): void {
  const groups = document.querySelectorAll<HTMLElement>(GROUPS);
  for (let i = 0; i < groups.length; i++) place(groups[i], false);
}

let listening = false;

/** Places every group's thumb, and the first time, follows them from then on. */
export function settleThumbs(): void {
  (window as ThumbsWindow).oeeeSettleThumbs = settle;
  settle();

  if (listening) return;
  listening = true;

  if (window.MutationObserver) {
    new MutationObserver((records) => {
      const moved: Group[] = [];
      for (let i = 0; i < records.length; i++) {
        const group = records[i].target.parentNode;
        if (isGroup(group) && moved.indexOf(group) < 0) moved.push(group);
      }
      for (let j = 0; j < moved.length; j++) place(moved[j], true);
    }).observe(document.documentElement, {
      subtree: true,
      attributes: true,
      attributeFilter: ["aria-current", "aria-pressed"],
    });
  }

  // A link in a pill that goes to another page, or to another view of this
  // one (pill_macro.jinja): chosen at once, so the thumb is on its way while
  // the page is. htmx has usually taken the click by now (a boosted link's is
  // cancelled), which still leaves this to do.
  document.addEventListener("click", (event) => {
    if (event.button !== 0) return;
    if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    const target = event.target as Element | null;
    const link = target && target.closest ? (target.closest("a[href]") as HTMLAnchorElement | null) : null;
    if (!link || link.target || link.origin !== location.origin) return;
    const boosted = link.closest('[hx-boost\\:inherited="true"]');
    if (event.defaultPrevented && !boosted) return;
    const group = link.parentNode;
    // The toolbar's sections are marked by the toolbar (oeeeMarkCurrent).
    if (!isGroup(group) || link.closest("#menubar")) return;
    if (link.getAttribute("aria-current") === "page") return;
    const was = chosen(group);
    group.oeeeWas = was;
    group.oeeeNow = link;
    if (was) was.removeAttribute("aria-current");
    link.setAttribute("aria-current", "page");
  });

  // htmx's history cache keeps the page being left as it stands when the
  // next arrives -- by when its pill already shows the next page's choice,
  // marked above. Kept like that, going back would find the wrong one chosen.
  // So the pill says the page's own choice for the keeping, and the new one
  // again straight after, in the same moment.
  function mark(group: Group, item: Element | null | undefined): void {
    const now = chosen(group);
    if (now && now !== item) now.removeAttribute("aria-current");
    if (item) item.setAttribute("aria-current", "page");
  }
  document.addEventListener("htmx:history:cache:before:save", () => {
    const groups = document.querySelectorAll<Group>("main .ds-segmented");
    for (let i = 0; i < groups.length; i++) {
      if ("oeeeWas" in groups[i]) mark(groups[i], groups[i].oeeeWas);
    }
  });
  document.addEventListener("htmx:history:cache:after:save", () => {
    const groups = document.querySelectorAll<Group>("main .ds-segmented");
    for (let i = 0; i < groups.length; i++) {
      if ("oeeeNow" in groups[i]) mark(groups[i], groups[i].oeeeNow);
      delete groups[i].oeeeWas;
      delete groups[i].oeeeNow;
    }
  });

  // Back to a page kept in memory: the choice is the one it was left with.
  window.addEventListener("pageshow", (event) => {
    if (!event.persisted) return;
    const groups = document.querySelectorAll<Group>(GROUPS);
    for (let i = 0; i < groups.length; i++) {
      const group = groups[i];
      if (!("oeeeWas" in group)) continue;
      const now = chosen(group);
      if (now && now !== group.oeeeWas) now.removeAttribute("aria-current");
      if (group.oeeeWas) group.oeeeWas.setAttribute("aria-current", "page");
      delete group.oeeeWas;
    }
    settle();
  });

  window.addEventListener("resize", settle);
  window.addEventListener("load", settle);
  if (document.fonts && document.fonts.ready) document.fonts.ready.then(settle);
  // A group inside a menu is measured when the menu opens.
  document.addEventListener("toggle", settle, true);
  document.addEventListener("htmx:after:settle", settle);
}
