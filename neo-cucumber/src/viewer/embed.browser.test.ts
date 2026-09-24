import { afterEach, describe, expect, it, vi } from "vitest";
import { mount } from "./embed";
import "./viewer.css";
import pageControls from "../../../templates/replay_controls.jinja?raw";

/**
 * A post page shows every drawing that has a replay with the replay's
 * controls already under it. What makes that affordable is that nothing is
 * fetched until one of them is used, and then once.
 */
describe("the viewer with a poster", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    document.body.textContent = "";
  });

  const mountWithPoster = () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    const fetchSpy = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(new Response(new ArrayBuffer(4), { status: 200 }));
    const viewer = mount(container, {
      replay: "/replay/ab/abc.pch",
      poster: "/image/ab/abc.png",
      width: 300,
      height: 200,
      lang: "en",
    });
    return { container, fetchSpy, viewer };
  };

  it("shows the drawing and its controls, and fetches nothing", () => {
    const { container, fetchSpy } = mountWithPoster();
    const poster = container.querySelector("img.neo-cucumber-replay-poster");
    expect(poster?.getAttribute("src")).toBe("/image/ab/abc.png");
    expect(container.querySelector(".neo-cucumber-replay-controls")).not.toBeNull();
    expect(container.querySelector("canvas")).toBeNull();
    // The drawing is finished, so the bar starts at its end.
    const seek = container.querySelector<HTMLInputElement>(".neo-cucumber-replay-seek");
    expect(seek?.value).toBe(seek?.max);
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("fetches once, however many controls are used while it loads", async () => {
    const { container, fetchSpy } = mountWithPoster();
    container.querySelector<HTMLButtonElement>(".neo-cucumber-replay-button")?.click();
    const seek = container.querySelector<HTMLInputElement>(".neo-cucumber-replay-seek");
    if (!seek) throw new Error("no seek bar");
    seek.value = "500";
    seek.dispatchEvent(new Event("input"));
    await vi.waitFor(() => {
      // Four bytes are not a replay: the failure is said, not thrown.
      expect(container.querySelector(".neo-cucumber-replay-status")?.textContent).toMatch(/could not be loaded/);
    });
    expect(fetchSpy).toHaveBeenCalledTimes(1);
  });

  it("plays when asked from outside, as a #replay address does", async () => {
    const { fetchSpy, viewer } = mountWithPoster();
    viewer.play();
    await vi.waitFor(() => expect(fetchSpy).toHaveBeenCalledTimes(1));
  });

  it("keeps the play button one width whether it shows Play or Pause", () => {
    const { container } = mountWithPoster();
    const button = container.querySelector<HTMLButtonElement>('[data-replay-control="play"]');
    if (!button) throw new Error("no play button");
    expect(button.getAttribute("aria-label")).toBe("Play");
    const { width, height } = button.getBoundingClientRect();
    button.setAttribute("data-playing", "");
    expect(button.getBoundingClientRect().width).toBe(width);
    expect(button.getBoundingClientRect().height).toBe(height);
  });
});

/**
 * The post page renders the controls with the drawing, disabled, so that
 * nothing under the stage moves when this script arrives and takes them over.
 */
describe("the viewer over a page's own controls", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    document.body.textContent = "";
  });

  // The template, as the server sends it: its comment is the server's alone.
  const rendered = pageControls
    .replace(/\{#[\s\S]*?#\}/g, "")
    .replace(/>\s+</g, "><")
    .trim();

  const page = () => {
    const container = document.createElement("div");
    // The page names the box as the viewer will, so its styles are there
    // from the first paint too.
    container.className = "neo-cucumber-replay";
    container.innerHTML =
      '<img class="neo-cucumber-replay-poster" width="300" height="200" alt="A drawing" src="/image/ab/abc.png">' + rendered;
    document.body.appendChild(container);
    return container;
  };

  const options = {
    replay: "/replay/ab/abc.pch",
    poster: "/image/ab/abc.png",
    width: 300,
    height: 200,
    lang: "en",
  };

  it("keeps them and the drawing where they were, and switches them on", () => {
    const container = page();
    const image = container.querySelector("img");
    const controls = container.querySelector(".neo-cucumber-replay-controls");
    if (!image || !controls) throw new Error("no page");
    const before = controls.getBoundingClientRect();
    expect(controls.querySelectorAll(":disabled")).toHaveLength(8);

    mount(container, options);

    expect(container.querySelector("img")).toBe(image);
    expect(container.querySelectorAll(".neo-cucumber-replay-controls")).toHaveLength(1);
    expect(container.querySelector(".neo-cucumber-replay-controls")).toBe(controls);
    expect(controls.getBoundingClientRect().toJSON()).toEqual(before.toJSON());
    expect(controls.querySelectorAll(":disabled")).toHaveLength(0);
    expect(
      controls.querySelector('[data-replay-control="skip"]')?.getAttribute("aria-label")
    ).toBe("Skip to end");
  });

  it("are the controls it would have made itself", () => {
    const own = document.createElement("div");
    document.body.appendChild(own);
    mount(own, options);
    const container = page();
    mount(container, options);
    expect(container.querySelector(".neo-cucumber-replay-controls")?.outerHTML).toBe(
      own.querySelector(".neo-cucumber-replay-controls")?.outerHTML
    );
  });
});
