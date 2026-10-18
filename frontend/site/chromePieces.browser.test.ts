// The smaller pieces of the site's chrome (chrome.ts): the Draw dialog, the
// toasts, the guesses at a press, the segmented controls' thumbs and the Per
// row slider. Each is set up the way its template's line sets it up, and
// again as a boosted swap would.
import { afterEach, describe, expect, it } from "vitest";
import { userEvent } from "vitest/browser";
import { attachAlert, attachConfirm } from "./alerts";
import { attachDrawDialog } from "./drawDialog";
import { installOptimistic } from "./optimistic";
import { attachPerRow } from "./perRow";
import { settleThumbs } from "./thumbs";
import { lookAtToasts } from "./toasts";

afterEach(() => {
  document.body.innerHTML = "";
});

describe("the Draw dialog", () => {
  const PAGE = `
    <form action="/draw" method="post"><button type="submit" id="nav-draw-button">Draw</button></form>
    <div id="draw-dialog" class="ds-modal" style="display: none;">
      <div class="ds-modal-panel">
        <h2 id="draw-dialog-title" tabindex="-1">New drawing</h2>
        <button type="button" data-draw-dialog-cancel>Cancel</button>
      </div>
    </div>`;

  function shown(): boolean {
    return document.getElementById("draw-dialog")!.style.display === "flex";
  }

  it("opens on the Draw button instead of submitting, and Escape puts focus back", async () => {
    document.body.innerHTML = PAGE;
    attachDrawDialog();
    const draw = document.getElementById("nav-draw-button")!;
    let submitted = 0;
    draw.closest("form")!.addEventListener("submit", (event) => {
      submitted++;
      event.preventDefault();
    });
    await userEvent.click(draw);
    expect(shown()).toBe(true);
    expect(submitted).toBe(0);
    expect(document.activeElement).toBe(document.getElementById("draw-dialog-title"));
    await userEvent.keyboard("{Escape}");
    expect(shown()).toBe(false);
    expect(document.activeElement).toBe(draw);
  });

  it("wires the dialog a boosted swap brings, and puts focus back from it", async () => {
    document.body.innerHTML = PAGE;
    attachDrawDialog();
    document.body.innerHTML = PAGE;
    attachDrawDialog();
    const draw = document.getElementById("nav-draw-button")!;
    await userEvent.click(draw);
    expect(shown()).toBe(true);
    await userEvent.click(document.querySelector("[data-draw-dialog-cancel]")!);
    expect(shown()).toBe(false);
    expect(document.activeElement).toBe(draw);
  });
});

describe("the toasts", () => {
  it("keeps the three newest, and times all but an error", () => {
    document.body.innerHTML = `<div id="toasts">${[1, 2, 3, 4]
      .map((n) => `<div class="ds-notice ds-notice-success" id="t${n}">${n}</div>`)
      .join("")}<div class="ds-notice ds-notice-error" id="e">no</div></div>`;
    lookAtToasts();
    const leaving = [...document.querySelectorAll(".is-leaving")].map((t) => t.id);
    expect(leaving).toEqual(["t1", "t2"]);
    expect(document.getElementById("t4")!.getAttribute("data-timed")).toBe("1");
    expect(document.getElementById("t4")!.className).toContain("ds-toast");
    expect(document.getElementById("e")!.getAttribute("data-timed")).toBe("1");
  });

  it("looks again for one put up by script", () => {
    document.body.innerHTML = `<div id="toasts"></div>`;
    lookAtToasts();
    document.getElementById("toasts")!.innerHTML = `<div class="ds-notice ds-notice-info" id="late">hi</div>`;
    (window as unknown as { oeeeToasts: { look(): void } }).oeeeToasts.look();
    expect(document.getElementById("late")!.getAttribute("data-timed")).toBe("1");
  });
});

describe("the guess at a press", () => {
  function request(name: string, ctx: object): void {
    document.dispatchEvent(new CustomEvent(name, { detail: { ctx } }));
  }

  it("fills a reaction chip and counts it at once, and takes it back when the server says no", () => {
    installOptimistic();
    document.body.innerHTML = `
      <div class="post-reactions">
        <button class="reaction-button" id="chip"><span class="reaction-count">2</span></button>
        <span class="reaction-total-count">5</span>
      </div>`;
    const chip = document.getElementById("chip")!;
    const ctx = { sourceElement: chip, response: { status: 500 } };
    request("htmx:before:request", ctx);
    expect(chip.classList.contains("reacted")).toBe(true);
    expect(chip.textContent).toBe("3");
    expect(document.querySelector(".reaction-total-count")!.textContent).toBe("6");
    request("htmx:after:request", ctx);
    expect(chip.classList.contains("reacted")).toBe(false);
    expect(chip.textContent).toBe("2");
    expect(document.querySelector(".reaction-total-count")!.textContent).toBe("5");
  });

  it("swaps a follow button's words, and back on an error", () => {
    installOptimistic();
    document.body.innerHTML = `
      <form id="follow" data-optimistic-toggle>
        <button class="ds-button ds-button-primary" data-pressed-label="Following">Follow</button>
      </form>`;
    const form = document.getElementById("follow")!;
    const button = form.querySelector("button")!;
    const ctx = { sourceElement: form };
    request("htmx:before:request", ctx);
    expect(button.textContent).toBe("Following");
    expect(button.classList.contains("ds-button-primary")).toBe(false);
    request("htmx:error", ctx);
    expect(button.textContent).toBe("Follow");
    expect(button.classList.contains("ds-button-primary")).toBe(true);
  });
});

describe("the segmented controls' thumbs", () => {
  it("puts the thumb under the chosen item, and marks a pill's link chosen at the press", async () => {
    // In a boosted region, as a page's pills are: htmx would take the click,
    // and this one is cancelled so the test page stays where it is.
    document.body.innerHTML = `
      <div hx-boost:inherited="true">
        <nav class="ds-segmented" id="pill" style="display: flex">
          <a href="?view=a" aria-current="page" style="padding: 0 10px">Drawings</a>
          <a href="?view=b" style="padding: 0 30px">Comments</a>
        </nav>
      </div>`;
    settleThumbs();
    const pill = document.getElementById("pill")!;
    const [a, b] = pill.querySelectorAll("a");
    expect(pill.classList.contains("has-thumb")).toBe(true);
    expect(pill.style.getPropertyValue("--thumb-w")).toBe(a.offsetWidth + "px");

    b.addEventListener("click", (event) => event.preventDefault());
    b.click();
    expect(b.getAttribute("aria-current")).toBe("page");
    expect(a.hasAttribute("aria-current")).toBe(false);
    // The observer moves the thumb, sliding.
    await expect.poll(() => pill.style.getPropertyValue("--thumb-x")).toBe(b.offsetLeft + "px");
    expect(pill.classList.contains("thumb-moves")).toBe(true);
  });
});

describe("the Per row slider", () => {
  it("sets the grid's count and keeps every slider for the same key in step", async () => {
    const control = (id: string) => `
      <div class="ds-per-row" data-per-row data-var="--test-cols" data-key="testCols" data-default="6">
        <input class="ds-range" type="range" id="${id}" min="1" max="12" step="1" />
        <output></output>
      </div>`;
    try {
      localStorage.removeItem("testCols");
    } catch {
      // Nothing kept.
    }
    document.body.innerHTML = control("one") + control("two");
    attachPerRow();
    const [one, two] = document.querySelectorAll<HTMLInputElement>("input[type=range]");
    expect(one.value).toBe("6");
    one.value = "10";
    one.dispatchEvent(new Event("input"));
    expect(document.documentElement.style.getPropertyValue("--test-cols")).toBe("10");
    expect(document.documentElement.getAttribute("data-testCols-density")).toBe("bare");
    expect(two.value).toBe("10");
    expect(document.querySelectorAll("output")[1].textContent).toBe("10");
    expect(localStorage.getItem("testCols")).toBe("10");
    localStorage.removeItem("testCols");
    document.documentElement.style.removeProperty("--test-cols");
    document.documentElement.removeAttribute("data-testCols-density");
  });
});

describe("the site's confirm", () => {
  const DIALOG = `
    <button id="opener">Delete</button>
    <div id="ds-confirm" style="display: none;">
      <p id="ds-confirm-message"></p>
      <button type="button" data-confirm-cancel>Cancel</button>
      <button type="button" data-confirm-ok>Delete</button>
    </div>
    <div id="ds-alert" style="display: none;">
      <p id="ds-alert-message"></p>
      <button type="button" data-alert-ok>OK</button>
    </div>`;

  it("asks once and is answered once, however many swaps came before", async () => {
    const felt: string[] = [];
    const host = window as unknown as { oeeeApp?: unknown };
    host.oeeeApp = { connected: () => true, feel: (name: string) => felt.push(name) };
    try {
      for (let swap = 0; swap < 3; swap++) {
        document.body.innerHTML = DIALOG;
        attachConfirm();
        attachAlert();
      }
      const opener = document.getElementById("opener")!;
      opener.focus();
      let issued = 0;
      let dropped = 0;
      // As htmx asks it: from the control, bubbling to the document.
      const question = new CustomEvent("htmx:confirm", {
        bubbles: true,
        cancelable: true,
        detail: { ctx: { confirm: "Delete it?" }, issueRequest: () => issued++, dropRequest: () => dropped++ },
      });
      opener.dispatchEvent(question);
      expect(question.defaultPrevented).toBe(true);
      const dialog = document.getElementById("ds-confirm")!;
      expect(dialog.style.display).toBe("flex");
      expect(document.getElementById("ds-confirm-message")!.textContent).toBe("Delete it?");
      expect(felt).toEqual(["warning"]);
      await userEvent.click(dialog.querySelector("[data-confirm-ok]")!);
      expect([issued, dropped]).toEqual([1, 0]);
      expect(dialog.style.display).toBe("none");
      expect(document.activeElement).toBe(opener);

      // A script's question, through the same dialog.
      const answer = (window as unknown as { dsConfirm(text: string): Promise<boolean> }).dsConfirm("Leave?");
      await userEvent.keyboard("{Escape}");
      expect(await answer).toBe(false);

      (window as unknown as { dsAlert(text: string): void }).dsAlert("Copied");
      const alert = document.getElementById("ds-alert")!;
      expect(alert.style.display).toBe("flex");
      await userEvent.keyboard("{Enter}");
      expect(alert.style.display).toBe("none");
    } finally {
      delete host.oeeeApp;
    }
  });
});
