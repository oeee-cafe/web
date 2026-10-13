import { useEffect, useRef } from "react";
import { Trans, useLingui } from "@lingui/react/macro";
import { NEO_FIELD, NEO_WELL } from "neo-cucumber";
import type { SaveDestinations } from "../../shared/saveDestinations";
import { mountCommunitySearch } from "../../shared/communitySearch";

export interface SaveDestinationProps {
  sessionId: string;
  destinations: SaveDestinations | null;
  /** The chosen community's id, or "" for the owner's own posts. */
  value: string;
  onChange: (community: string) => void;
}

/**
 * Where Save to Gallery will post: said, for a room in a community, and
 * chosen here for one opened from the lobby, with the search box the drafts
 * page uses too (shared/communitySearch.ts). Until the suggestions arrive,
 * and if they never do, the save posts to the owner's own posts.
 */
export const SaveDestination = ({ sessionId, destinations, value, onChange }: SaveDestinationProps) => {
  const { t } = useLingui();
  const host = useRef<HTMLDivElement>(null);
  // Read when the box is built, not followed: the box keeps its own choice
  // and reports it, and rebuilding it on every report would lose the field.
  const latest = useRef({ value, onChange });
  latest.current = { value, onChange };
  const choices = destinations && !destinations.community ? destinations.choices : null;

  useEffect(() => {
    if (!host.current || !choices) return;
    return mountCommunitySearch(host.current, {
      sessionId,
      suggestions: choices,
      value: latest.current.value,
      onChange: (community) => latest.current.onChange(community),
      id: "save-destination",
      words: {
        search: t`Search communities`,
        ownPosts: t`My own posts`,
        member: t`member`,
        drawn: t`drawn here`,
        noMatches: t`No matching communities`,
      },
      classes: {
        root: "flex flex-col gap-[4px] text-left",
        input: `${NEO_FIELD} w-full px-[4px] py-[3px] text-[12px]`,
        list: `${NEO_WELL} max-h-[132px] overflow-y-auto py-[2px]`,
        option:
          "flex cursor-pointer items-baseline justify-between gap-[6px] px-[4px] py-[2px] " +
          "aria-selected:bg-(--neo-text) aria-selected:text-(--neo-bk) " +
          "data-active:outline-1 data-active:-outline-offset-1 data-active:outline-(--neo-icon-select)",
        meta: "shrink-0 px-[4px] text-[11px] opacity-70",
      },
    });
  }, [sessionId, choices, t]);

  if (!destinations) return null;
  if (destinations.community) {
    const name = destinations.community.name;
    return (
      <p className="mb-[12px] text-[12px]">
        <Trans>Posts to {name}</Trans>
      </p>
    );
  }
  return (
    <div className="mb-[12px] text-[12px]">
      <label htmlFor="save-destination" className="mb-[4px] block text-left">
        <Trans>Post to</Trans>
      </label>
      <div ref={host} />
    </div>
  );
};
