import { Trans } from "@lingui/react/macro";
import { useEffect } from "react";

import { askSteam } from "../../shared/appBridge";
import type { Steam } from "../hooks/useSteam";
import { FriendList } from "./FriendList";

/**
 * Inviting Steam friends into the room, filling the friends window
 * (FriendsWindow): the friends the Steam build lists, those in Oeee Cafe
 * first. An invitation is Steam's own, sent by the app (steam.rs in
 * oeee-cafe-desktop); a friend who accepts has Steam open their app on the
 * room, so only friends with Oeee Cafe on Steam can come in this way.
 */
export const SteamInvite = ({ steam }: { steam: Steam }) => {
  // Asked for each time the picker opens.
  useEffect(() => {
    askSteam("friends");
  }, []);

  return (
    <FriendList
      friends={steam.friends}
      invitations={steam.invitations}
      invite={steam.invite}
      empty={<Trans>No Steam friends to invite yet.</Trans>}
    />
  );
};
