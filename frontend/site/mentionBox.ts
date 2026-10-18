// The `@` autocomplete in a comment box: typing an @ and the start of
// somebody's name lists who it could be, under the box (.combobox,
// style.css), and picking one writes their @login_name in.
//
// Who is listed is the server's to say (GET data-mention-url?q=, which is
// models::people): a handle typed exactly, whoever it is, and the people
// near the writer by the start of a name. What is asked about is an @ that
// does not continue a word (crate::mention's rule) and what follows it up
// to a space or a punctuation mark: a login name, or the start of a display
// name in any script -- `@그림` as much as `@neo`. What is written in is
// always the login name, which is all a mention is.
//
// The arrow keys move through the list, Enter or Tab picks, Escape closes
// it. While it is closed the keys are the box's own.
//
// A piece of chrome.js (attachMentions), which post_view.jinja calls for a
// signed-in reader.

/** One row of the server's answer. */
export interface Person {
  login_name: string;
  display_name: string;
}

/** The @name being typed: where it starts and ends, and what is asked. */
interface Token {
  start: number;
  end: number;
  q: string;
}

const DELAY = 150; // ms of no typing before the server is asked
const NAME = /[A-Za-z0-9_-]/;
const BEFORE_WORD = /[\w.@-]/; // an @ after one of these continues a word
// Spaces and punctuation outside ASCII: general punctuation, CJK and
// fullwidth. Any other character past ASCII is taken for a letter, as
// the server's char::is_alphanumeric takes Hangul and Han; Firefox 56 has
// no \p{L} to ask.
const WIDE_GAP =
  /[\s\u2000-\u206f\u3000-\u303f\uff01-\uff0f\uff1a-\uff20\uff3b-\uff40\uff5b-\uff65]/;
const MAX_NAME = 50;

// A letter past ASCII: Hangul, kana, Han and the rest.
function isWideLetter(c: string): boolean {
  return c.charCodeAt(0) > 127 && !WIDE_GAP.test(c);
}

function continuesWord(c: string): boolean {
  return BEFORE_WORD.test(c) || isWideLetter(c);
}

// What may follow the @: a login name's letters, or a display name's.
function isQueryChar(c: string): boolean {
  return NAME.test(c) || isWideLetter(c);
}

// The option a press or a move landed on, if any.
function optionOf(target: EventTarget | null, list: HTMLElement): HTMLElement | null {
  let item = target as HTMLElement | null;
  while (item && item !== list && !item.hasAttribute("data-index")) {
    item = item.parentNode as HTMLElement | null;
  }
  return item && item !== list ? item : null;
}

export function attachMention(box: HTMLTextAreaElement): void {
  const url = box.getAttribute("data-mention-url");
  if (!url || box.getAttribute("data-mention-ready")) return;
  box.setAttribute("data-mention-ready", "1");

  const listId = box.id + "-mentions";
  const list = document.createElement("ul");
  list.id = listId;
  list.className = "combobox-list mention-list";
  list.setAttribute("role", "listbox");
  list.setAttribute("aria-label", box.getAttribute("data-mention-label") || "");
  list.style.display = "none";
  box.parentNode!.insertBefore(list, box.nextSibling);

  box.setAttribute("aria-autocomplete", "list");
  box.setAttribute("aria-controls", listId);
  box.setAttribute("aria-expanded", "false");

  let people: Person[] = [];
  let active = -1;
  let token: Token | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let asked = 0; // the newest request; older answers are dropped

  // The @name the caret is in or just after, or null. Hangul being
  // composed is in the box already, so `@그` is asked about mid-syllable.
  function current(): Token | null {
    if (box.selectionStart !== box.selectionEnd) return null;
    const text = box.value;
    const caret = box.selectionStart;
    let at = caret;
    while (at > 0 && isQueryChar(text.charAt(at - 1))) at--;
    if (at === 0 || text.charAt(at - 1) !== "@") return null;
    const start = at - 1;
    if (start > 0 && continuesWord(text.charAt(start - 1))) return null;
    let end = caret;
    while (end < text.length && isQueryChar(text.charAt(end))) end++;
    if (end - start - 1 > MAX_NAME) return null;
    return { start, end, q: text.slice(start + 1, caret) };
  }

  function close(): void {
    people = [];
    active = -1;
    token = null;
    list.style.display = "none";
    list.innerHTML = "";
    box.setAttribute("aria-expanded", "false");
    box.removeAttribute("aria-activedescendant");
  }

  function highlight(index: number): void {
    active = index;
    const items = list.children;
    for (let i = 0; i < items.length; i++) {
      if (i === index) {
        items[i].setAttribute("data-active", "");
        items[i].setAttribute("aria-selected", "true");
      } else {
        items[i].removeAttribute("data-active");
        items[i].setAttribute("aria-selected", "false");
      }
    }
    const item = index >= 0 ? (items[index] as HTMLElement | undefined) : undefined;
    if (item) {
      box.setAttribute("aria-activedescendant", item.id);
      if (item.offsetTop < list.scrollTop) list.scrollTop = item.offsetTop;
      else if (item.offsetTop + item.offsetHeight > list.scrollTop + list.clientHeight) {
        list.scrollTop = item.offsetTop + item.offsetHeight - list.clientHeight;
      }
    } else {
      box.removeAttribute("aria-activedescendant");
    }
  }

  function show(found: Person[]): void {
    people = found;
    list.innerHTML = "";
    if (!people.length) {
      close();
      return;
    }
    for (let i = 0; i < people.length; i++) {
      const item = document.createElement("li");
      item.id = listId + "-" + i;
      item.className = "combobox-option";
      item.setAttribute("role", "option");
      item.setAttribute("data-index", String(i));
      const name = document.createElement("span");
      name.className = "combobox-option-name";
      name.textContent = people[i].display_name;
      const handle = document.createElement("span");
      handle.className = "combobox-option-meta";
      handle.textContent = "@" + people[i].login_name;
      item.appendChild(name);
      item.appendChild(handle);
      list.appendChild(item);
    }
    list.style.display = "";
    box.setAttribute("aria-expanded", "true");
    highlight(0);
  }

  function ask(q: string): void {
    const mine = ++asked;
    const request = new XMLHttpRequest();
    request.open("GET", url + "?q=" + encodeURIComponent(q));
    request.setRequestHeader("Accept", "application/json");
    request.onload = () => {
      if (mine !== asked || !token) return;
      if (request.status !== 200) return close();
      let found: Person[] | null;
      try {
        found = JSON.parse(request.responseText);
      } catch {
        return close();
      }
      show(found || []);
    };
    request.send();
  }

  function update(): void {
    const next = current();
    if (!next) {
      clearTimeout(timer);
      asked++;
      close();
      return;
    }
    const same = token && token.start === next.start && token.q === next.q;
    token = next;
    if (same) return;
    clearTimeout(timer);
    timer = setTimeout(() => {
      ask(token ? token.q : "");
    }, DELAY);
  }

  function pick(index: number): void {
    const person = people[index];
    const at = current() || token;
    if (!person || !at) return close();
    const text = box.value;
    const after = text.slice(at.end);
    const written = "@" + person.login_name + (after.charAt(0) === " " ? "" : " ");
    box.value = text.slice(0, at.start) + written + after;
    const caret = at.start + written.length + (after.charAt(0) === " " ? 1 : 0);
    box.setSelectionRange(caret, caret);
    asked++;
    close();
    box.focus();
  }

  box.addEventListener("input", update);
  box.addEventListener("click", update);
  box.addEventListener("keyup", (e) => {
    // The caret moved without typing.
    if (e.key === "ArrowLeft" || e.key === "ArrowRight" || e.key === "Home" || e.key === "End") {
      update();
    }
  });
  box.addEventListener("keydown", (e) => {
    if (list.style.display === "none" || !people.length) return;
    // Keys an input method is composing with are its own.
    if (e.isComposing || e.keyCode === 229) return;
    if (e.key === "ArrowDown" || e.key === "Down") {
      e.preventDefault();
      highlight((active + 1) % people.length);
    } else if (e.key === "ArrowUp" || e.key === "Up") {
      e.preventDefault();
      highlight((active - 1 + people.length) % people.length);
    } else if (e.key === "Enter" || e.key === "Tab") {
      if (active < 0) return;
      e.preventDefault();
      pick(active);
    } else if (e.key === "Escape" || e.key === "Esc") {
      e.preventDefault();
      e.stopPropagation();
      asked++;
      close();
    }
  });
  box.addEventListener("blur", () => {
    asked++;
    close();
  });
  // A press on the list keeps the box focused, so blur does not close it
  // before the click lands.
  list.addEventListener("mousedown", (e) => {
    e.preventDefault();
  });
  list.addEventListener("click", (e) => {
    const item = optionOf(e.target, list);
    if (item) pick(parseInt(item.getAttribute("data-index")!, 10));
  });
  list.addEventListener("mousemove", (e) => {
    const item = optionOf(e.target, list);
    if (item) {
      const index = parseInt(item.getAttribute("data-index")!, 10);
      if (index !== active) highlight(index);
    }
  });
  // A form that is reset, as one is once its comment is in, has no @ left.
  if (box.form) {
    box.form.addEventListener("reset", () => {
      asked++;
      close();
    });
  }
}

function attachAllMentions(): void {
  const boxes = document.querySelectorAll<HTMLTextAreaElement>("textarea[data-mention-url]");
  for (let i = 0; i < boxes.length; i++) attachMention(boxes[i]);
}

/** Every comment box with a mention list, once the page is read. */
export function attachMentions(): void {
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", attachAllMentions);
  } else {
    attachAllMentions();
  }
}
