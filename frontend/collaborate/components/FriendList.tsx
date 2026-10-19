import { Trans, useLingui } from "@lingui/react/macro";
import { NEO_BUTTON, NEO_BUTTON_ON, NEO_FIELD, NEO_WELL } from "neo-cucumber";
import { useMemo, useState, type ReactNode } from "react";

import type { DiscordFriend } from "../../shared/appBridge";
import type { InvitationFate } from "../hooks/useDiscord";

/** The friends window's well, which the list and what comes before it fill. */
export const FRIENDS_PANEL = `${NEO_WELL} m-[3px] flex min-h-0 flex-1 flex-col p-[4px] text-[11px] leading-[15px]`;

const STATUS_DOT: Record<DiscordFriend["status"], string> = {
  online: "bg-green-500",
  idle: "bg-yellow-500",
  dnd: "bg-red-500",
  offline: "bg-gray-400",
};

export interface FriendListProps {
  /** As the app sent them, or null before it has. */
  friends: DiscordFriend[] | null;
  /** By the friend's id. */
  invitations: Record<string, InvitationFate>;
  invite: (id: string) => void;
  /** What to say when there is nobody to invite at all. */
  empty: ReactNode;
}

/**
 * The friends to invite into the room, Discord's or Steam's, in the order
 * the app sent them -- those in Oeee Cafe first -- found by name. Once an
 * invitation is sent a friend's button says so, so a second press sends
 * nothing twice.
 */
export const FriendList = ({ friends, invitations, invite, empty }: FriendListProps) => {
  const { t } = useLingui();
  const [query, setQuery] = useState("");

  const shown = useMemo(() => {
    const wanted = query.trim().toLowerCase();
    return (friends ?? []).filter((friend) => !wanted || friend.name.toLowerCase().includes(wanted));
  }, [friends, query]);

  return (
    <div className={FRIENDS_PANEL}>
      <input
        type="search"
        value={query}
        onChange={(event) => setQuery(event.target.value)}
        placeholder={t`Search friends`}
        aria-label={t`Search friends`}
        className={`${NEO_FIELD} mb-[4px] w-full shrink-0 px-[3px] text-[11px] leading-[15px]`}
      />
      <ul className="min-h-0 flex-1 overflow-y-auto">
        {friends === null ? (
          <li className="opacity-70">
            <Trans>Loading friends…</Trans>
          </li>
        ) : shown.length === 0 ? (
          <li className="opacity-70">{query ? <Trans>No friend by that name.</Trans> : empty}</li>
        ) : (
          shown.map((friend) => {
            const fate = invitations[friend.id];
            return (
              <li
                key={friend.id}
                className={`flex items-center gap-[4px] py-[1px] ${friend.status === "offline" ? "opacity-60" : ""}`}
              >
                <span className={`h-[6px] w-[6px] shrink-0 rounded-full ${STATUS_DOT[friend.status]}`} />
                <span className="min-w-0 flex-1 truncate">
                  {friend.name}
                  {friend.playing && (
                    <span className="opacity-60">
                      {" · "}
                      <Trans>in Oeee Cafe</Trans>
                    </span>
                  )}
                </span>
                <button
                  type="button"
                  disabled={fate === "sending" || fate === "sent"}
                  onClick={() => invite(friend.id)}
                  className={`${NEO_BUTTON} shrink-0 !px-[5px] !py-0 ${fate === "sent" ? NEO_BUTTON_ON : ""}`}
                >
                  {fate === "sending" ? (
                    <Trans>Sending…</Trans>
                  ) : fate === "sent" ? (
                    <Trans>Sent</Trans>
                  ) : fate === "failed" ? (
                    <Trans>Try again</Trans>
                  ) : (
                    <Trans>Invite</Trans>
                  )}
                </button>
              </li>
            );
          })
        )}
      </ul>
    </div>
  );
};
