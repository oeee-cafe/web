// The reader's Discord account in the Windows app, on /account
// (account_discord_app.jinja): shown only where the app has said where it
// stands (bridge.ts's discord.state), never in a browser or another app.

import type { OeeeApp } from "../bridge";

/** The section's words: `connected` has "{name}" where the name goes. */
export interface DiscordWords {
  off: string;
  connecting: string;
  connected: string;
}

interface Linked {
  connected?: boolean;
  connecting?: boolean;
  name?: string | null;
}

export function discordAccount(WORDS: DiscordWords): void {
  const section = document.getElementById("account-discord-app");
  const app = (window as Window & typeof globalThis & { oeeeApp?: OeeeApp }).oeeeApp;
  const discord = app && app.discord;
  if (!section || !discord) return;
  const status = section.querySelector("[data-discord-status]")!;
  const connect = section.querySelector<HTMLButtonElement>("[data-discord-connect]")!;
  const disconnect = section.querySelector<HTMLButtonElement>("[data-discord-disconnect]")!;

  // Connected, the account is named by the reader's language's own wording --
  // Chinese puts a full-width colon after "Discord" -- with the name the app
  // says put where "{name}" is. A function, not a string, as the
  // replacement: a name with "$&" in it stays as it is.
  function named(name: string): string {
    return WORDS.connected.replace("{name}", () => name);
  }

  function show(linked: Linked | null): void {
    if (!linked || !document.body.contains(section)) return;
    section!.hidden = false;
    if (linked.connected) status.textContent = linked.name ? named(linked.name) : "Discord";
    else status.textContent = linked.connecting ? WORDS.connecting : WORDS.off;
    disconnect.hidden = !linked.connected;
    connect.hidden = !!linked.connected;
    connect.disabled = !!linked.connecting;
  }

  connect.addEventListener("click", () => discord.ask("connect"));
  disconnect.addEventListener("click", () => discord.ask("disconnect"));
  document.addEventListener("oeee:discord", (event) => {
    const detail = (event as CustomEvent<{ kind: string; value: Linked }>).detail;
    if (detail && detail.kind === "state") show(detail.value);
  });
  show(discord.known().state as Linked | null);
}
