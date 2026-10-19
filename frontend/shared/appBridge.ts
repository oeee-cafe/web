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

/**
 * A Discord friend who pressed Ask to Join on the reader's activity in
 * Discord, waiting to be let into the room.
 */
export interface DiscordRequest {
  /** A Discord id, as a friend's is. */
  id: string;
  name: string;
}

export type DiscordHeard =
  | { kind: "state"; value: DiscordLinked }
  | { kind: "friends"; value: DiscordFriend[] }
  | { kind: "requests"; value: DiscordRequest[] }
  | { kind: "invited"; value: { user: string; sent: boolean } };

/** What the page asks of the reader's Discord account (app_bridge.jinja). */
export type DiscordAction = "connect" | "disconnect" | "friends" | "invite" | "letIn" | "dismiss";

interface DiscordKnown {
  state: DiscordLinked | null;
  friends: DiscordFriend[] | null;
  requests?: DiscordRequest[] | null;
}

interface DiscordMembers {
  known(): DiscordKnown;
  ask(action: DiscordAction, user?: string): boolean;
}

function discordMembers(): DiscordMembers | undefined {
  return (window as unknown as { oeeeApp?: { discord?: DiscordMembers } }).oeeeApp?.discord;
}

/**
 * What the Windows app last said of the reader's Discord account and friends
 * (app_bridge.jinja keeps it). A state of null is a page no app with Discord
 * has spoken to, where nothing about Discord is offered.
 */
export function discordKnown(): { state: DiscordLinked | null; friends: DiscordFriend[] | null; requests: DiscordRequest[] } {
  const known = discordMembers()?.known();
  return { state: known?.state ?? null, friends: known?.friends ?? null, requests: known?.requests ?? [] };
}

/** Hears whatever the app says of Discord from now on. Returns the stop. */
export function onDiscord(listener: (heard: DiscordHeard) => void): () => void {
  const heard = (event: Event) => listener((event as CustomEvent<DiscordHeard>).detail);
  document.addEventListener("oeee:discord", heard);
  return () => document.removeEventListener("oeee:discord", heard);
}

/** Asks the app something of the reader's Discord account; false outside it. */
export function askDiscord(action: DiscordAction, user?: string): boolean {
  return discordMembers()?.ask(action, user) ?? false;
}

/**
 * A Steam friend the reader can invite into the room, as the Steam build
 * of the Windows app lists them (steam.rs in oeee-cafe-desktop): the same
 * as a Discord friend, an id a Steam id.
 */
export type SteamFriend = DiscordFriend;

export type SteamHeard =
  | { kind: "friends"; value: SteamFriend[] }
  | { kind: "invited"; value: { user: string; sent: boolean } };

/** What the page asks of Steam (app_bridge.jinja). */
export type SteamAction = "friends" | "invite";

interface SteamMembers {
  known(): { friends: SteamFriend[] | null };
  ask(action: SteamAction, user?: string, join?: string): boolean;
}

function steamMembers(): SteamMembers | undefined {
  const app = (window as unknown as { oeeeApp?: { connected(): boolean; steam?: SteamMembers } }).oeeeApp;
  return app && app.connected() ? app.steam : undefined;
}

/**
 * Whether Steam's friends can be offered: in the Steam build of the Windows
 * app only (data-store="steam", theme_head.jinja), which is the one with
 * Steam to ask (steam.rs in oeee-cafe-desktop).
 */
export function steamCanInvite(): boolean {
  return document.documentElement.getAttribute("data-store") === "steam" && steamMembers() !== undefined;
}

/** The Steam friends the app last listed (app_bridge.jinja keeps them), or null. */
export function steamFriendsKnown(): SteamFriend[] | null {
  return steamCanInvite() ? (steamMembers()?.known().friends ?? null) : null;
}

/** Hears whatever the app says of Steam from now on. Returns the stop. */
export function onSteam(listener: (heard: SteamHeard) => void): () => void {
  const heard = (event: Event) => listener((event as CustomEvent<SteamHeard>).detail);
  document.addEventListener("oeee:steam", heard);
  return () => document.removeEventListener("oeee:steam", heard);
}

/** Asks the Steam build something; false outside it. */
export function askSteam(action: SteamAction, user?: string, join?: string): boolean {
  return steamMembers()?.ask(action, user, join) ?? false;
}
