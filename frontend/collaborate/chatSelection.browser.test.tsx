import { afterEach, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { I18nProvider } from "@lingui/react";
import { i18n } from "@lingui/core";
import { DefaultI18n, NEO_BUTTON } from "neo-cucumber";
import { Chat } from "./components/Chat";
import { setupI18n } from "./i18n";
import "./app.css";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

/**
 * The session page refuses selection, and the transcript is what it lets back
 * in.
 *
 * A message is the one thing on a drawing page that is there to be read, so
 * it can be dragged across and copied. Everything around it -- the buttons
 * beside the log, and the toolbox the drag would otherwise reach -- stays
 * unselectable, because a selection is a mode that eats the next press and
 * nothing on a page of buttons ever clears it.
 */

let host: HTMLElement | null = null;
let root: Root | null = null;

type AddMessage = (message: {
  id: string;
  type: "join" | "leave" | "user";
  userId: string;
  username: string;
  message: string;
  timestamp: number;
}) => void;

async function renderChat() {
  setupI18n("en");
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);

  let addMessage: AddMessage | null = null;
  await act(async () => {
    root!.render(
      <I18nProvider i18n={i18n} defaultComponent={DefaultI18n}>
        <Chat
          wsRef={{ current: null }}
          userId="1"
          participants={new Map()}
          connectionState="connected"
          onChatMessage={() => {}}
          onAddMessage={(fn) => (addMessage = fn)}
        />
        <button type="button" className={NEO_BUTTON}>
          Save
        </button>
      </I18nProvider>,
    );
  });

  await act(async () => {
    (addMessage as AddMessage | null)?.({
      id: "1",
      type: "user",
      userId: "2",
      username: "someone",
      message: "worth copying",
      timestamp: Date.now(),
    });
  });

  return host;
}

afterEach(() => {
  if (root) act(() => root?.unmount());
  root = null;
  host?.remove();
  host = null;
});

describe("the collaborative session page", () => {
  it("lets a message be selected, and nothing around it", async () => {
    const rendered = await renderChat();

    const said = [...rendered.querySelectorAll<HTMLElement>("span")].find(
      (el) => el.textContent === "worth copying",
    )!;
    expect(getComputedStyle(said).userSelect).toBe("text");

    // The chrome the drag would otherwise cross.
    const button = document.body.querySelector<HTMLElement>("button")!;
    expect(getComputedStyle(button).userSelect).toBe("none");
  });

  it("keeps the message input typable, which is why fields are excepted", async () => {
    const rendered = await renderChat();
    const field = rendered.querySelector<HTMLInputElement>("input")!;

    expect(getComputedStyle(field).userSelect).toBe("text");
  });
});
