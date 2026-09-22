import { afterEach, describe, expect, it, vi } from "vitest";
import { mount } from "./embed";
import "./viewer.css";

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

  it("keeps the play button one width whether it says Play or Pause", () => {
    const { container } = mountWithPoster();
    const button = container.querySelector<HTMLButtonElement>(".neo-cucumber-replay-button");
    const [play, pause] = Array.from(
      container.querySelectorAll<HTMLElement>(".neo-cucumber-replay-label")
    );
    if (!button || !play || !pause) throw new Error("no play button");
    // Only the current label is read out, and only it is seen.
    expect(play.getAttribute("aria-hidden")).toBe("false");
    expect(pause.getAttribute("aria-hidden")).toBe("true");
    expect(pause.getBoundingClientRect().height).toBe(0);
    const width = button.getBoundingClientRect().width;
    const height = button.getBoundingClientRect().height;
    play.setAttribute("aria-hidden", "true");
    pause.setAttribute("aria-hidden", "false");
    expect(button.getBoundingClientRect().width).toBe(width);
    expect(button.getBoundingClientRect().height).toBe(height);
  });
});
