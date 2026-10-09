-- STEAM_SUPPORTER is for buying Oeee Cafe on Steam, and a refund takes it
-- back. Every other achievement is still never taken back.
--
-- steam_owner is the Steam account whose purchase earned the row: not
-- necessarily the account's linked Steam identity, because the Steam app
-- hands over a ticket for whoever is signed in. It is what the daily
-- recheck asks Steam about (steam::recheck_app_purchases), and
-- steam_checked_at is when it last did, NULL being never.
--
-- One purchase earns it for one account, as a Supporter Pack belongs to one:
-- earning it on another moves it, and the row it leaves is revoked with
-- steam_owner cleared, so no recheck brings it back there.
--
-- revoked_at is set when Steam stops saying it is owned, and cleared when
-- it says so again. The row stays so that Steam can be told:
-- steam_synced_at goes back to NULL, and the sync sends the achievement as
-- locked (value 0) rather than unlocked. A revoked row is not shown.
--
-- All three are new and nullable, so the release serving while this boots
-- reads and writes the table as before. For that moment it would show a
-- revoked row and, were one unsynced, send it to Steam as unlocked; nothing
-- is revoked until this release's recheck runs.
ALTER TABLE user_achievements
  ADD COLUMN steam_owner text,
  ADD COLUMN steam_checked_at timestamptz,
  ADD COLUMN revoked_at timestamptz;

-- The ones granted before now were for owning a Supporter Pack, by the
-- linked Steam account. Ask about the app for that same account.
UPDATE user_achievements a
SET steam_owner = i.subject
FROM user_identities i
WHERE a.achievement = 'STEAM_SUPPORTER'
  AND i.user_id = a.user_id
  AND i.provider = 'steam';

CREATE INDEX user_achievements_steam_checked ON user_achievements (steam_checked_at NULLS FIRST)
  WHERE steam_owner IS NOT NULL;
