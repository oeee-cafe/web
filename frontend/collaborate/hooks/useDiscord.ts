import { useEffect, useState } from "react";

import {
  askDiscord,
  discordKnown,
  onDiscord,
  type DiscordFriend,
  type DiscordLinked,
} from "../../shared/appBridge";

/** An invitation's fate: on its way, sent, or not. */
export type InvitationFate = "sending" | "sent" | "failed";

/**
 * Discord, as the Windows app last said: whether the reader's account is
 * connected, and their friends once asked for. A state of null is a page
 * no app with Discord spoke to -- a browser, another app -- where the room
 * offers nothing of Discord at all.
 */
export function useDiscord() {
  const [state, setState] = useState<DiscordLinked | null>(() => discordKnown().state);
  const [friends, setFriends] = useState<DiscordFriend[] | null>(() => discordKnown().friends);
  // By the friend's id.
  const [invitations, setInvitations] = useState<Record<string, InvitationFate>>({});

  useEffect(
    () =>
      onDiscord((heard) => {
        if (heard.kind === "state") setState(heard.value);
        else if (heard.kind === "friends") setFriends(heard.value);
        else {
          const { user, sent } = heard.value;
          setInvitations((current) => ({ ...current, [user]: sent ? "sent" : "failed" }));
        }
      }),
    [],
  );

  const invite = (id: string) => {
    if (askDiscord("invite", id)) setInvitations((current) => ({ ...current, [id]: "sending" }));
  };

  return { state, friends, invitations, invite };
}

export type Discord = ReturnType<typeof useDiscord>;

/** The friends window's size as it opens: wide enough for a name beside its
 *  Invite button, at the chat's text size (DiscordFriendsWindow). */
export const FRIENDS_WINDOW_SIZE = { width: 220, height: 260 };

/**
 * Whether a friend could be asked into this room at all: the site says how
 * to join one only when anyone signed in may enter it (presence.rs), and
 * Discord's invitation is that.
 */
export function roomIsJoinable(): boolean {
  return roomJoin() !== null;
}

/** The path that joins this room, as presence.rs says it, or null. */
export function roomJoin(): string | null {
  return document.querySelector('meta[name="oeee-presence"][data-join]')?.getAttribute("data-join") || null;
}
