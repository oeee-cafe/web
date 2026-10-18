// Publishing a drawing (post_form.jinja) and moving a post to another
// community (post_edit_community.jinja).

type FormWindow = Window & typeof globalThis & { oeeeApp?: { connected(): boolean; feel(name: string): void } };

/**
 * The tag field's suggestion menu, once the page is read. It used to answer a
 * click and nothing else, inside a container that announced itself as a
 * listbox -- so anyone working from the keyboard, and anyone on a screen
 * reader, had a menu they could neither reach nor dismiss.
 */
export function tagSuggestions(): void {
  document.addEventListener("DOMContentLoaded", () => {
    const input = document.getElementById("tags-input") as HTMLInputElement | null;
    const menu = document.getElementById("tag-suggestions");
    if (!input || !menu) return;

    // The separators the server splits on, so the tag being typed is the
    // same one either side sees. The full-width forms are what an IME emits.
    const SEPARATORS = /[\s,、，#＃]+/;
    let active = -1;

    function options(): HTMLElement[] {
      return Array.prototype.slice.call(menu!.querySelectorAll(".tag-autocomplete-item"));
    }

    function highlight(index: number): void {
      const items = options();
      active = items.length ? (index + items.length) % items.length : -1;
      items.forEach((item, i) => {
        const on = i === active;
        item.classList.toggle("is-active", on);
        item.setAttribute("aria-selected", on ? "true" : "false");
      });
      input!.setAttribute("aria-activedescendant", active >= 0 ? items[active].id : "");
    }

    function close(): void {
      menu!.innerHTML = "";
      active = -1;
      input!.setAttribute("aria-expanded", "false");
      input!.setAttribute("aria-activedescendant", "");
    }

    function choose(item: HTMLElement): void {
      const tags = input!.value.split(SEPARATORS).filter((t) => t.trim());
      tags.pop(); // the partial tag this suggestion replaces
      tags.push(item.dataset.tag!);
      input!.value = tags.join(", ") + ", ";
      close();
      const app = (window as FormWindow).oeeeApp;
      if (app && app.connected()) app.feel("selection");
      input!.focus();
    }

    menu.addEventListener("click", (e) => {
      const item = (e.target as Element).closest<HTMLElement>(".tag-autocomplete-item");
      if (item) choose(item);
    });

    menu.addEventListener("mousemove", (e) => {
      const item = (e.target as Element).closest<HTMLElement>(".tag-autocomplete-item");
      if (item) highlight(options().indexOf(item));
    });

    input.addEventListener("keydown", (e) => {
      const items = options();
      if (e.key === "Escape") {
        close();
        return;
      }
      if (!items.length) return;
      if (e.key === "ArrowDown") {
        e.preventDefault();
        highlight(active + 1);
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        highlight(active - 1);
      } else if (e.key === "Enter" && active >= 0) {
        // Only when a suggestion is highlighted: otherwise Enter still
        // submits the post, which is what it does with the menu closed.
        e.preventDefault();
        choose(items[active]);
      }
    });

    // A fresh batch of suggestions starts with nothing highlighted. Kept on
    // htmx 2's name for the event, as it was written; htmx 4 calls it
    // htmx:after:swap, so this does not run today.
    menu.addEventListener("htmx:afterSwap", () => {
      active = -1;
      input!.setAttribute("aria-expanded", options().length ? "true" : "false");
      input!.setAttribute("aria-activedescendant", "");
    });

    document.addEventListener("click", (e) => {
      if (!menu!.contains(e.target as Node) && e.target !== input) close();
    });
  });
}

/** Moving a post: choosing a community from a list that a search replaces. */
export function moveToCommunity(): void {
  const form = document.getElementById("move-form") as HTMLFormElement;
  const submit = document.getElementById("move-submit") as HTMLButtonElement;
  const search = document.getElementById("community-search")!;

  // Choosing a row names it on the button.
  form.addEventListener("change", (event) => {
    const radio = event.target as HTMLInputElement;
    if (radio.name !== "community_id") return;
    submit.disabled = false;
    submit.textContent = radio.getAttribute("data-move-label");
    submit.setAttribute("data-chosen", radio.value);
    const rows = form.querySelectorAll(".move-row");
    for (let i = 0; i < rows.length; i++) {
      rows[i].classList.toggle("is-chosen", rows[i].contains(radio));
    }
  });
  // The radio is out of sight, so its row shows where the keyboard is.
  form.addEventListener("focusin", (event) => {
    const target = event.target as Element;
    const row = target.closest && target.closest(".move-row");
    if (row) row.classList.add("has-focus");
  });
  form.addEventListener("focusout", (event) => {
    const target = event.target as Element;
    const row = target.closest && target.closest(".move-row");
    if (row) row.classList.remove("has-focus");
  });

  // A new list, from the search: the row chosen before is chosen again when
  // it is in it, and otherwise nothing is, so the button never names a row
  // that is not on the page. It is remembered, so a search that brings it
  // back brings the choice back too.
  document.addEventListener("htmx:after:swap", () => {
    const chosen = submit.getAttribute("data-chosen");
    if (chosen === null) return;
    const radios = form.querySelectorAll<HTMLInputElement>("input[name=community_id]");
    for (let i = 0; i < radios.length; i++) {
      if (radios[i].value === chosen) {
        radios[i].checked = true;
        radios[i].dispatchEvent(new Event("change", { bubbles: true }));
        return;
      }
    }
    submit.disabled = true;
    submit.textContent = submit.getAttribute("data-choose");
  });

  // Enter in the search box would send the form with whatever was chosen
  // before; it picks the first row left instead.
  search.addEventListener("keydown", (event) => {
    if (event.key !== "Enter") return;
    event.preventDefault();
    const first = form.querySelector<HTMLInputElement>(".move-row input");
    if (first) {
      first.checked = true;
      first.dispatchEvent(new Event("change", { bubbles: true }));
      first.focus();
    }
  });
}
