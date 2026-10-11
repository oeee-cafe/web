-- Deleting an account now gives up its login name (models/user.rs,
-- delete_user). Accounts deleted before that still hold theirs; give those up
-- the same way, with a name nobody could have registered in advance.
--
-- Those deleted before password_hash became nullable (add_user_identities)
-- were left with '' in it, where later ones have NULL; both mean no password,
-- and this makes them all say it the same way.
UPDATE users
SET login_name = 'deleted_' || replace(gen_random_uuid()::text, '-', ''),
    password_hash = NULL,
    updated_at = NOW()
WHERE deleted_at IS NOT NULL
  AND login_name !~ '^deleted_[0-9a-f]{32}$';

UPDATE actors
SET username = users.login_name,
    name = '[deleted]',
    handle = '@' || users.login_name || '@' || actors.handle_host,
    url = regexp_replace(actors.url, '/@[^/]*$', '/@' || users.login_name),
    updated_at = NOW()
FROM users
WHERE actors.user_id = users.id
  AND users.deleted_at IS NOT NULL
  AND actors.username <> users.login_name;
