-- Sign-ins that are not a password: Steam first, then Microsoft and Apple.
--
-- One table for every provider, one row per account a person has linked. A
-- provider names its users by a subject -- a SteamID64, Apple's `sub`,
-- Microsoft's tenant and object ids -- and that pair is what signs someone in,
-- so it is unique across the site. A person links at most one account from
-- each provider.
--
-- `provider` is text under a CHECK rather than an enum: adding a provider is
-- then a constraint swapped in a migration, where an enum needs
-- ALTER TYPE ... ADD VALUE and cannot use the value in the transaction that
-- added it.
CREATE TABLE user_identities (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  provider text NOT NULL CONSTRAINT user_identities_provider_check CHECK (provider IN ('steam')),
  subject text NOT NULL,
  -- What the provider called the person when they last signed in (a Steam
  -- persona name, an email address), to tell linked accounts apart on the
  -- account page. Never used to sign anyone in.
  display_hint text,
  email varchar(320),
  created_at timestamptz NOT NULL DEFAULT now(),
  last_used_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (provider, subject),
  UNIQUE (user_id, provider)
);

-- An account made by signing in with a provider has no password until its
-- owner sets one. verify_password fails on NULL, so password sign-in simply
-- does not work for it.
--
-- Deploys are blue/green, and the release this replaces reads password_hash
-- as non-null: while both colours are up, the old one fails to load an
-- account the new one made without a password. That is one sign-in's worth
-- of window for an account that did not exist before this release.
ALTER TABLE users ALTER COLUMN password_hash DROP NOT NULL;

