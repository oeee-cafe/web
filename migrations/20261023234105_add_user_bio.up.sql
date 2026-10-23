-- What a person says about themselves on their profile, as they typed it.
-- The actor's bio_html is made from it whenever it changes.
ALTER TABLE users ADD COLUMN bio TEXT NOT NULL DEFAULT '';
ALTER TABLE users ADD CONSTRAINT ck_users_bio_length CHECK (char_length(bio) <= 1000);
