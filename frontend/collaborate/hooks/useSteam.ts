import { useEffect, useState } from "react";

import { askSteam, onSteam, steamCanInvite, steamFriendsKnown, type SteamFriend } from "../../shared/appBridge";
import { roomJoin, type InvitationFate } from "./useDiscord";

/**
 * Steam, in the Steam build of the Windows app: whether there is Steam to
 * ask, and the friends once asked for. Anywhere else -- a browser, another
 * app or store -- the room offers nothing of Steam.
 */
export function useSteam() {
  const ready = steamCanInvite();
  const [friends, setFriends] = useState<SteamFriend[] | null>(steamFriendsKnown);
  // By the friend's id.
  const [invitations, setInvitations] = useState<Record<string, InvitationFate>>({});

  useEffect(
    () =>
      onSteam((heard) => {
        if (heard.kind === "friends") setFriends(heard.value);
        else {
          const { user, sent } = heard.value;
          setInvitations((current) => ({ ...current, [user]: sent ? "sent" : "failed" }));
        }
      }),
    [],
  );

  // Into the room showing; Steam hands the friend who accepts its path.
  const invite = (id: string) => {
    const join = roomJoin();
    if (join && askSteam("invite", id, join)) setInvitations((current) => ({ ...current, [id]: "sending" }));
  };

  return { ready, friends, invitations, invite };
}

export type Steam = ReturnType<typeof useSteam>;
