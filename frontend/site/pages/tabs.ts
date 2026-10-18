// Tabs a page switches in place: the community directory's lists
// (communities.jinja) and a profile's drawings and comments (profile.jinja).

/**
 * The community directory's tabs, the one chosen last remembered in this
 * browser. Runs where it stands, before the page is painted, so a remembered
 * view arrives as itself rather than flashing the public list first.
 */
export function communitiesTabs(): void {
  const tabs = document.querySelectorAll("[data-communities-tab]");
  function show(chosen: string): void {
    let found = false;
    for (let i = 0; i < tabs.length; i++) {
      if (tabs[i].getAttribute("data-communities-tab") === chosen) found = true;
    }
    if (!found) chosen = "public";
    for (let j = 0; j < tabs.length; j++) {
      tabs[j].setAttribute("aria-pressed", String(tabs[j].getAttribute("data-communities-tab") === chosen));
    }
    const parts = document.querySelectorAll<HTMLElement>("[data-communities-panel], [data-communities-for]");
    for (let k = 0; k < parts.length; k++) {
      const owner = parts[k].getAttribute("data-communities-panel") || parts[k].getAttribute("data-communities-for");
      parts[k].hidden = owner !== chosen;
    }
  }
  let saved: string | null = null;
  try {
    saved = localStorage.getItem("communitiesTab");
  } catch {
    // No storage: the public list.
  }
  if (saved) show(saved);
  for (let i = 0; i < tabs.length; i++) {
    tabs[i].addEventListener("click", function (this: Element) {
      const chosen = this.getAttribute("data-communities-tab")!;
      show(chosen);
      try {
        localStorage.setItem("communitiesTab", chosen);
      } catch {
        // Shown, and not remembered.
      }
    });
  }
}

// Found at the time rather than kept: a boosted visit to another profile
// swaps the body, and the listeners below outlive it.
function selectProfileTab(chosen: string): void {
  const tabs = document.querySelectorAll("[data-profile-tab]");
  for (let j = 0; j < tabs.length; j++) {
    if (tabs[j].getAttribute("data-profile-tab") === chosen) {
      tabs[j].setAttribute("aria-current", "page");
    } else {
      tabs[j].removeAttribute("aria-current");
    }
  }
  const panels = document.querySelectorAll<HTMLElement>("[data-profile-panel]");
  for (let k = 0; k < panels.length; k++) {
    panels[k].hidden = panels[k].getAttribute("data-profile-panel") !== chosen;
  }
  const notes = document.querySelectorAll<HTMLElement>("[data-profile-note]");
  for (let n = 0; n < notes.length; n++) {
    notes[n].hidden = notes[n].getAttribute("data-profile-note") !== chosen;
  }
  // How many a row is a question about drawings only.
  const perRow = document.querySelector<HTMLElement>("[data-profile-per-row]");
  if (perRow) perRow.hidden = chosen === "comments";
}

let profilePopstate = false;

/** A profile's tabs, each its own address. */
export function profileTabs(): void {
  const tabs = document.querySelectorAll<HTMLAnchorElement>("[data-profile-tab]");
  for (let i = 0; i < tabs.length; i++) {
    tabs[i].addEventListener("click", function (this: HTMLAnchorElement, event) {
      // A new tab or window, or a copied link, is the link's.
      if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      event.preventDefault();
      if (this.getAttribute("aria-current") === "page") return;
      selectProfileTab(this.getAttribute("data-profile-tab")!);
      history.pushState({ profileTab: true }, "", this.href);
    });
  }

  // Back and forward between tabs. htmx restores only the entries it pushed,
  // so these are left to here; the one the page was loaded at is htmx's, and
  // it fetches that address afresh.
  if (profilePopstate) return;
  profilePopstate = true;
  window.addEventListener("popstate", () => {
    const all = document.querySelectorAll<HTMLAnchorElement>("[data-profile-tab]");
    for (let i = 0; i < all.length; i++) {
      if (all[i].pathname === location.pathname) {
        selectProfileTab(all[i].getAttribute("data-profile-tab")!);
        return;
      }
    }
  });
}
