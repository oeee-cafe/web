-- Pickles: a thank-you rarer than a reaction. A supporter -- someone who
-- owns this year's Supporter Pack -- has one pickle a week to give, to
-- somebody else's post on this site. The week is Seoul's, Monday to Sunday,
-- and a pickle not given by Sunday night is gone: nothing is stored until it
-- is given, so an unused one never exists and there is no balance to expire.
--
-- `source` is where a given pickle came from. 'weekly' is the only one now;
-- pickles bought on their own may come later, and they will be 'purchased',
-- pointing at the purchase they were paid from, with a balance worked out
-- from those purchases. Weekly pickles never count towards that balance, and
-- a giver who has both spends the weekly one first, since it is the one that
-- runs out. On a post a pickle is a pickle: the source is the giver's
-- business, and showing it would show who paid.
--
-- Text under a CHECK rather than an enum, as with supporter_purchases.store:
-- adding a source is then an ALTER of the check.
CREATE TABLE pickles (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    giver_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    post_id uuid NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    source text NOT NULL
        CONSTRAINT pickles_source_check CHECK (source IN ('weekly')),
    -- The Monday, in Seoul, of the week a weekly pickle was that week's.
    week date,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT pickles_week_iff_weekly CHECK ((source = 'weekly') = (week IS NOT NULL)),
    -- One from each giver to a post: a thank-you, not a tip jar.
    CONSTRAINT pickles_giver_post_key UNIQUE (giver_id, post_id)
);

-- One weekly pickle a week, held here rather than by a count in the handler,
-- so two presses at once cannot both get through.
CREATE UNIQUE INDEX pickles_one_weekly ON pickles (giver_id, week)
    WHERE source = 'weekly';

CREATE INDEX pickles_post_id ON pickles (post_id);
