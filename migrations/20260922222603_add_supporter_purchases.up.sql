-- Every Supporter Pack anyone has bought, one row per purchase.
--
-- A pack is a year's. Buying 2026's supports Oeee Cafe through 2026, and
-- supporting again means buying 2027's when it comes. Neither store sells
-- the same thing twice -- a Steam DLC is owned once and for good, and so is
-- a non-consumable in the App Store -- so a year is a product of its own on
-- both, which is what `steam.supporter_apps` and
-- `app_store.supporter_products` name. The mark beside someone's name is
-- this year's pack; a profile shows every year they have bought.
--
-- **Whose it is.** The row names the Oeee Cafe account, not an identity:
-- buying does not mean signing in with the platform you bought on. Someone
-- signed in with a password inside the Steam app buys the DLC and the Steam
-- app hands over a ticket; someone signed in with a password inside the iOS
-- app buys the pack and the app hands over the transaction. Neither has to
-- link anything.
--
-- **What stops it counting twice.** The key is the purchase as the platform
-- knows it -- `provider` and `owner` and `product` -- and not the account,
-- so one purchase is one row however often it is restored or handed over.
-- `owner` is the Steam account that owns the DLC, or the App Store's
-- original transaction id, which is the only name Apple gives a purchase.
-- Restoring in the iOS app, or signing into a different account in the Steam
-- app, updates that one row: the pack moves, and no second account keeps it.
--
-- `revoked_at` is set when the platform stops saying it is owned -- a refund
-- -- and cleared if it says so again. The row stays either way, so a
-- transaction is still there to ask about.
CREATE TABLE supporter_purchases (
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    provider text NOT NULL
        CONSTRAINT supporter_purchases_provider_check CHECK (provider IN ('steam', 'apple')),
    owner text NOT NULL,
    product text NOT NULL,
    year integer NOT NULL,
    purchased_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz,
    checked_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (provider, owner, product)
);

-- Who is supporting in a given year, which is what every page that prints a
-- name asks, and what a profile lists.
CREATE INDEX supporter_purchases_live ON supporter_purchases (user_id, year)
  WHERE revoked_at IS NULL;

-- The App Store recheck's worklist: Apple's purchases, longest unasked
-- first. Steam is asked about by account, not by purchase.
CREATE INDEX supporter_purchases_checked ON supporter_purchases (checked_at)
  WHERE provider = 'apple';

-- The supporters there already are, so nobody's mark disappears between this
-- deploy and the first recheck. The old standing said only that a Steam
-- account owned something in steam.supporter_app_ids, never which pack, so
-- the row stands in for one: it supports the year it was bought in, and the
-- first recheck replaces it with the packs Steam actually names -- revoking
-- this one, because it is not among them.
INSERT INTO supporter_purchases (user_id, provider, owner, product, year, purchased_at)
SELECT user_id, provider, subject, 'legacy',
       EXTRACT(year FROM supporter_since AT TIME ZONE 'Asia/Seoul')::integer,
       supporter_since
FROM user_identities
WHERE supporter_since IS NOT NULL;

-- user_identities.supporter_since stays where it is: the previous release
-- reads it on every page it renders, and both colours serve during a deploy.
-- Nothing writes it any more -- standing has a year, and belongs to an
-- account rather than to an identity, so a column on user_identities cannot
-- hold it, and what it says will go stale. Dropping it is a migration for a
-- later deploy, once no serving release reads it.
--
-- supporter_checked_at keeps its job either way: it is when Steam was last
-- asked about that account, and the recheck still works through it so a
-- linked account that owns nothing is not asked about every ten minutes for
-- ever.
