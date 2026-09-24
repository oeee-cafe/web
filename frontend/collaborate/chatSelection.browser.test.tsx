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
 * The session page refuses selection, the transcript included.
 *
 * Messages used to be the one exception, so they could be copied out, and on
 * iOS that meant a long press anywhere in the chat raised the loupe and a
 * selection over the log -- in the window people keep open while they draw.
 * So nothing on the page selects except the places you type; see
 * frontend/shared/selection.css. This pins that the transcript stayed out of
 * the exception after it was taken away: for a while the stylesheet said one
 * thing and this test the other.
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
  it("lets nothing be selected, the messages included", async () => {
    const rendered = await renderChat();

    const said = [...rendered.querySelectorAll<HTMLElement>("span")].find(
      (el) => el.textContent === "worth copying",
    )!;
    expect(getComputedStyle(said).userSelect).toBe("none");

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
