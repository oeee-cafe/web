-- Every post drawn so far has had its replay open to anyone who asked for it,
-- so the column defaults to that: only a post the author has since opted out of
-- is false. The recording itself is always kept — this decides who may watch it.
ALTER TABLE posts
ADD COLUMN allow_replay BOOLEAN NOT NULL DEFAULT true;
