import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { userEvent } from "vitest/browser";
import { attachMention, type Person } from "./mentionBox";

const PEOPLE: Person[] = [
  { login_name: "neo", display_name: "Neo" },
  { login_name: "nemo", display_name: "Nemo" },
  { login_name: "nell", display_name: "넬" },
];

/** Every `q` the box asked the server about. */
let asked: string[] = [];

/** The server, answering whoever's login name starts with what was typed. */
class FakeRequest {
  status = 0;
  responseText = "";
  onload: (() => void) | null = null;
  private q = "";
  open(_method: string, url: string) {
    this.q = decodeURIComponent(url.split("?q=")[1] ?? "");
  }
  setRequestHeader() {}
  send() {
    asked.push(this.q);
    setTimeout(() => {
      this.status = 200;
      this.responseText = JSON.stringify(PEOPLE.filter((p) => p.login_name.startsWith(this.q)));
      this.onload?.();
    }, 0);
  }
}

function box(): HTMLTextAreaElement {
  return document.querySelector("textarea")!;
}

function list(): HTMLUListElement {
  return document.getElementById("comment-mentions") as HTMLUListElement;
}

function shown(): boolean {
  return list().style.display !== "none" && list().children.length > 0;
}

function activeName(): string | null {
  const active = list().querySelector("[data-active]");
  return active ? active.querySelector(".combobox-option-meta")!.textContent : null;
}

/** Types into the box and waits for the list the server's answer makes. */
async function typeAndList(text: string): Promise<void> {
  await userEvent.keyboard(text);
  await expect.poll(shown).toBe(true);
}

beforeEach(() => {
  asked = [];
  vi.stubGlobal("XMLHttpRequest", FakeRequest);
  document.body.innerHTML = `
    <form>
      <textarea id="comment" data-mention-url="/api/people" data-mention-label="People"></textarea>
    </form>`;
  attachMention(box());
  box().focus();
});

afterEach(() => {
  vi.unstubAllGlobals();
  document.body.innerHTML = "";
});

it("lists who an @ could be, the first one ready to pick", async () => {
  await typeAndList("hi @ne");
  expect(asked).toEqual(["ne"]);
  expect(list().children.length).toBe(3);
  expect(activeName()).toBe("@neo");
  expect(box().getAttribute("aria-expanded")).toBe("true");
  expect(box().getAttribute("aria-activedescendant")).toBe("comment-mentions-0");
});

it("moves through the list with the arrow keys, round from either end", async () => {
  await typeAndList("@ne");
  await userEvent.keyboard("{ArrowDown}");
  expect(activeName()).toBe("@nemo");
  await userEvent.keyboard("{ArrowDown}{ArrowDown}");
  expect(activeName()).toBe("@neo");
  await userEvent.keyboard("{ArrowUp}");
  expect(activeName()).toBe("@nell");
  // The caret did not go anywhere while the list had the arrows.
  expect(box().selectionStart).toBe(3);
});

it("writes the login name in on Enter, not a new line", async () => {
  await typeAndList("hi @ne");
  await userEvent.keyboard("{ArrowDown}{Enter}");
  expect(box().value).toBe("hi @nemo ");
  expect(box().selectionStart).toBe("hi @nemo ".length);
  expect(shown()).toBe(false);
  expect(box().getAttribute("aria-expanded")).toBe("false");
});

it("writes the login name in on Tab and keeps the box focused", async () => {
  await typeAndList("@nel");
  await userEvent.keyboard("{Tab}");
  expect(box().value).toBe("@nell ");
  expect(document.activeElement).toBe(box());
});

it("replaces the whole name the caret is in, and reuses the space after it", async () => {
  box().value = "@nexyz and more";
  box().setSelectionRange(3, 3);
  box().dispatchEvent(new Event("input"));
  await expect.poll(shown).toBe(true);
  await userEvent.keyboard("{Enter}");
  expect(box().value).toBe("@neo and more");
  expect(box().selectionStart).toBe("@neo ".length);
});

it("closes on Escape, and the keys are the box's own again", async () => {
  await typeAndList("@ne");
  await userEvent.keyboard("{Escape}");
  expect(shown()).toBe(false);
  await userEvent.keyboard("{Enter}");
  expect(box().value).toBe("@ne\n");
});

it("leaves the keys to an input method that is composing", async () => {
  await typeAndList("@ne");
  const composing = new KeyboardEvent("keydown", {
    key: "ArrowDown",
    isComposing: true,
    bubbles: true,
    cancelable: true,
  });
  box().dispatchEvent(composing);
  expect(composing.defaultPrevented).toBe(false);
  expect(activeName()).toBe("@neo");
});

it("asks nothing for an @ in the middle of a word", async () => {
  await userEvent.keyboard("mail@ne");
  await new Promise((resolve) => setTimeout(resolve, 300));
  expect(asked).toEqual([]);
  expect(shown()).toBe(false);
});

it("asks about a name in any script", async () => {
  await userEvent.keyboard("@넬");
  await expect.poll(() => asked).toEqual(["넬"]);
});
