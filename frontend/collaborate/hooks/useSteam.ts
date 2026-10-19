import { useEffect, useState } from "react";

import { askSteam, onSteam, steamKnown, type SteamFriend } from "../../shared/appBridge";
import { roomJoin, type InvitationFate } from "./useDiscord";

/**
 * Steam, as the Steam build of the Windows app last said: whether it can
 * list the reader's Steam friends, and the friends once asked for. Not
 * ready is anywhere else -- a browser, another app or store, or a Steam
 * build from before the picker -- where the room offers nothing of Steam.
 */
export function useSteam() {
  const [ready, setReady] = useState(() => steamKnown().ready);
  const [friends, setFriends] = useState<SteamFriend[] | null>(() => steamKnown().friends);
  // By the friend's id.
  const [invitations, setInvitations] = useState<Record<string, InvitationFate>>({});

  useEffect(
    () =>
      onSteam((heard) => {
        if (heard.kind === "ready") setReady(steamKnown().ready);
        else if (heard.kind === "friends") setFriends(heard.value);
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
