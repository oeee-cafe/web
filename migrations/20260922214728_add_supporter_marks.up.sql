-- Which platform's mark a supporter wears beside their name.
--
-- Standing is per identity (see 20260922081941_add_supporters), so someone
-- who buys the Supporter Pack on Steam and again from the App Store supports
-- on two platforms at once. Their profile names both either way; this is the
-- one the rest of the site prints beside their name.
--
-- NULL -- the default, and everyone who supports on one platform -- means
-- the platform they supported on first. A name here is honoured only while
-- that platform still grants standing, so a refund moves the mark to the
-- other platform rather than leaving them wearing a store they no longer own.
--
-- Text under a CHECK rather than an enum for the reason
-- 20260922003339_add_user_identities gives about user_identities_provider_check:
-- adding a provider is then an ALTER and not a type change. The two lists have
-- to name the same providers, and a test in models/supporter.rs holds them
-- together.
ALTER TABLE users ADD COLUMN supporter_mark text
  CONSTRAINT users_supporter_mark_check CHECK (supporter_mark IN ('steam', 'apple'));
