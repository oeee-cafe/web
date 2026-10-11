import type { PainterCommand, PainterHandle } from "neo-cucumber";

/**
 * The painter, offered to the apps once it is ready (Pencil.swift and
 * Scripts.swift in oeee-cafe-apple; any app on the bridge can drive it). The app drives it from what the Apple Pencil says --
 * double-tap and squeeze, as the reader set them in Settings -- and tells it
 * when the system's "Only Draw with Apple Pencil" is on. Anywhere else nothing
 * is listening, and nothing happens.
 *
 * Both drawing pages make the offer, the painter's and the collaborative
 * session's, which is why it lives beside them rather than in either: a pen
 * does not stop being a pen because other people are drawing too.
 *
 * Returns the offer's withdrawal, for a host that unmounts its painter.
 */
export function offerPainterToApp(painter: PainterHandle): () => void {
  const host = window as unknown as {
    oeeeApp?: {
      post(type: string, data: Record<string, unknown>): boolean;
      painter?: { command(name: PainterCommand): void; preferPen(): void };
    };
  };
  const app = host.oeeeApp;
  if (!app) return () => {};
  const offered = {
    command: (name: PainterCommand) => painter.command(name),
    preferPen: () => painter.preferPen(),
  };
  app.painter = offered;
  // Said once the painter can be driven, so the app never has to guess when
  // that is (app_bridge.jinja).
  app.post("painter", { state: "ready" });
  return () => {
    if (app.painter === offered) delete app.painter;
  };
}

/** The haptics the apps play, as theme_head.jinja names them. */
export type Haptic = "light" | "medium" | "selection" | "success" | "warning" | "error";

/**
 * A press or an outcome felt in the app, for what the page's own listeners
 * (theme_head.jinja) cannot see: a painter's chrome, or a request made with
 * fetch rather than htmx, whose result is known only here. Nothing happens
 * outside an app.
 */
export function feelInApp(name: Haptic): void {
  const app = (window as unknown as { oeeeApp?: { connected(): boolean; feel(name: string): void } }).oeeeApp;
  if (app && app.connected()) app.feel(name);
}

/** The reader's Discord account in the Windows app (discord.rs in oeee-cafe-desktop). */
export interface DiscordLinked {
  connected: boolean;
  /** Asking Discord, or trading what it gave: not yet either way. */
  connecting: boolean;
  name: string | null;
}

/** A Discord friend the reader can invite into the room. */
export interface DiscordFriend {
  /** A Discord id: a string, since a number cannot hold one. */
  id: string;
  name: string;
  status: "online" | "idle" | "dnd" | "offline";
  /** In Oeee Cafe now. */
  playing: boolean;
}

export type DiscordHeard =
  | { kind: "state"; value: DiscordLinked }
  | { kind: "friends"; value: DiscordFriend[] }
  | { kind: "invited"; value: { user: string; sent: boolean } };

interface DiscordMembers {
  known(): { state: DiscordLinked | null; friends: DiscordFriend[] | null };
  ask(action: "connect" | "disconnect" | "friends" | "invite", user?: string): boolean;
}

function discordMembers(): DiscordMembers | undefined {
  return (window as unknown as { oeeeApp?: { discord?: DiscordMembers } }).oeeeApp?.discord;
}

/**
 * What the Windows app last said of the reader's Discord account and friends
 * (app_bridge.jinja keeps it). A state of null is a page no app with Discord
 * has spoken to, where nothing about Discord is offered.
 */
export function discordKnown(): { state: DiscordLinked | null; friends: DiscordFriend[] | null } {
  return discordMembers()?.known() ?? { state: null, friends: null };
}

/** Hears whatever the app says of Discord from now on. Returns the stop. */
export function onDiscord(listener: (heard: DiscordHeard) => void): () => void {
  const heard = (event: Event) => listener((event as CustomEvent<DiscordHeard>).detail);
  document.addEventListener("oeee:discord", heard);
  return () => document.removeEventListener("oeee:discord", heard);
}

/** Asks the app something of the reader's Discord account; false outside it. */
export function askDiscord(action: "connect" | "disconnect" | "friends" | "invite", user?: string): boolean {
  return discordMembers()?.ask(action, user) ?? false;
}
