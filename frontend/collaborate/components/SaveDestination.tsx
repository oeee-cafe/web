import { Trans, useLingui } from "@lingui/react/macro";
import { NEO_FIELD } from "neo-cucumber";
import type { SaveDestinations } from "../../shared/saveDestinations";

export interface SaveDestinationProps {
  destinations: SaveDestinations | null;
  /** The chosen community's id, or "" for the owner's own posts. */
  value: string;
  onChange: (community: string) => void;
}

/**
 * Where Save to Gallery will post: said, for a room in a community, and
 * chosen here for one opened from the lobby. Until the choices arrive, and
 * if they never do, the save posts to the owner's own posts.
 */
export const SaveDestination = ({ destinations, value, onChange }: SaveDestinationProps) => {
  const { t } = useLingui();
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
    <div className="mb-[12px] flex flex-wrap items-center justify-center gap-[6px] text-[12px]">
      <label htmlFor="save-destination">
        <Trans>Post to</Trans>
      </label>
      <select
        id="save-destination"
        value={value}
        onChange={(event) => onChange(event.target.value)}
        className={`${NEO_FIELD} max-w-full px-[4px] py-[2px] text-[12px] cursor-pointer`}
      >
        {/* An <option> holds text only, so the words come from t, not <Trans>. */}
        <option value="">{t`My own posts`}</option>
        {destinations.choices.map((place) => (
          <option key={place.id} value={place.id}>
            {place.name}
          </option>
        ))}
      </select>
    </div>
  );
};
