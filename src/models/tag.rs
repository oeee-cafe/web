use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// A tag, with the counts the `tag_stats` view derives for it.
///
/// `post_count` is not stored: `tags.post_count` is a leftover counter that
/// nothing maintains any more (see the merge_and_normalize_tags migration).
/// Every query below reads the view instead, so the number a tag shows and the
/// posts its page lists come from one definition of what a tag counts.
#[derive(Clone, Debug, Serialize)]
pub struct Tag {
    pub id: Uuid,
    pub name: String,
    pub display_name: String,
    pub post_count: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Longest tag we keep. `tags.name` is varchar(255), so this is a product
/// decision rather than a storage one: past about this length a tag is a
/// sentence, and truncating beats failing the publish it arrived with.
pub const MAX_TAG_LENGTH: usize = 60;

/// Most tags one post may carry. Anything past this is dropped.
pub const MAX_TAGS_PER_POST: usize = 30;

/// True for the characters a tag may contain.
///
/// Letters, digits and `_`, which is what a URL path segment can hold without
/// escaping and what the rest of the fediverse accepts. `is_alphanumeric` is
/// Unicode-aware, so 그림, イラスト and рисунок are all tags; `/`, `?`, `#` and
/// `%` — each of which used to produce a tag whose own link 404ed or 400ed —
/// are not.
fn is_tag_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// True where one tag ends and the next begins.
///
/// `#` is a separator rather than a character to strip, so `#art#drawing` and
/// `#art` both come apart correctly — people type the hash, and it used to be
/// kept, producing a tag named `#art` that rendered as `##art` and linked to
/// `/tags/#art`, which is a fragment. The full-width comma, ideographic
/// comma and full-width hash are here because the placeholder text is
/// translated into Japanese, Korean and Chinese and an IME does not emit the
/// ASCII ones; `is_whitespace` covers U+3000 for the same reason.
fn is_tag_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, ',' | '，' | '、' | '#' | '＃')
}

/// Parse the tag field as typed into (normalized name, display name) pairs.
///
/// Normalizing drops anything `is_tag_char` rejects rather than refusing
/// the tag: this runs inside a publish, and losing a stray character is a much
/// better outcome than losing the post. The pairs come back in the order they
/// were typed, deduplicated by normalized name, so `Art art` is one tag.
pub fn parse_tag_input(input: &str) -> Vec<(String, String)> {
    let mut parsed: Vec<(String, String)> = Vec::new();

    for token in input.split(is_tag_separator) {
        let display: String = token
            .replace('-', "_")
            .chars()
            .filter(|c| is_tag_char(*c))
            .take(MAX_TAG_LENGTH)
            .collect();
        if display.is_empty() {
            continue;
        }
        let name = display.to_lowercase();
        if parsed.iter().any(|(existing, _)| *existing == name) {
            continue;
        }
        parsed.push((name, display));
        if parsed.len() == MAX_TAGS_PER_POST {
            break;
        }
    }

    parsed
}

/// Normalize one tag that arrived from outside the tag field — a URL path
/// segment, a search box — the same way `parse_tag_input` normalizes one it
/// parsed, so `/tags/Art` and `/tags/art` are the same page.
pub fn normalize_tag(raw: &str) -> String {
    raw.replace('-', "_")
        .chars()
        .filter(|c| is_tag_char(*c))
        .take(MAX_TAG_LENGTH)
        .collect::<String>()
        .to_lowercase()
}

/// Replace a post's tags with the ones in `input`.
///
/// `None` leaves them alone, for an API client that predates the field;
/// `Some("")` clears them. All three call sites that publish or edit a post go
/// through here, so what the tag field means is decided once.
pub async fn set_post_tags(
    tx: &mut Transaction<'_, Postgres>,
    post_id: Uuid,
    input: Option<&str>,
) -> Result<()> {
    let Some(input) = input else {
        return Ok(());
    };

    unlink_post_tags(tx, post_id).await?;
    link_post_to_tags(tx, post_id, &parse_tag_input(input)).await
}

/// Link a post to tags, creating the ones that do not exist yet.
///
/// Private, with `unlink` below: `set_post_tags` is the only way in, so
/// there is one place that decides what replacing a post's tags means.
///
/// One statement regardless of how many tags there are, and no counter to keep
/// in step. It upserts rather than selecting-then-inserting because two people
/// publishing the same new tag at the same moment used to race, and the loser's
/// unique violation aborted their entire publish.
async fn link_post_to_tags(
    tx: &mut Transaction<'_, Postgres>,
    post_id: Uuid,
    tags: &[(String, String)], // (normalized_name, display_name), in the order typed
) -> Result<()> {
    if tags.is_empty() {
        return Ok(());
    }

    let names: Vec<String> = tags.iter().map(|(name, _)| name.clone()).collect();
    let display_names: Vec<String> = tags
        .iter()
        .map(|(_, display_name)| display_name.clone())
        .collect();
    let positions: Vec<i16> = (0..tags.len() as i16).collect();

    // `DO UPDATE`, not `DO NOTHING`: RETURNING skips the rows a DO NOTHING
    // conflict discarded, and the second insert needs an id for every tag,
    // whether this call created it or found it. An existing tag keeps the
    // display_name it was first written with.
    sqlx::query!(
        r#"
        WITH input AS (
            SELECT *
            FROM UNNEST($2::varchar[], $3::varchar[], $4::smallint[])
                AS t(name, display_name, position)
        ), upserted AS (
            INSERT INTO tags (name, display_name)
            SELECT name, display_name FROM input
            ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name
            RETURNING id, name
        )
        INSERT INTO post_tags (post_id, tag_id, position)
        SELECT $1, upserted.id, input.position
        FROM upserted
        JOIN input USING (name)
        ON CONFLICT (post_id, tag_id) DO NOTHING
        "#,
        post_id,
        &names,
        &display_names,
        &positions
    )
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// Remove every tag from a post.
///
/// Deleting a post does *not* call this. Deletion is soft, and the view that
/// counts a tag's posts already excludes a deleted one, so the links are left
/// in place — which is what lets a restored post keep the tags it was published
/// with. It also means the three delete paths that never called this (a remote
/// Delete, a community cascade, an account closing) no longer need to.
async fn unlink_post_tags(tx: &mut Transaction<'_, Postgres>, post_id: Uuid) -> Result<()> {
    sqlx::query!("DELETE FROM post_tags WHERE post_id = $1", post_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// One of a post's own tags.
///
/// Deliberately not a `Tag`: a post page shows the tag's name and links to
/// it, and never shows how many other posts carry it, so this runs on the most
/// requested page on the site without joining the counts view to answer a
/// question nobody asked.
#[derive(Clone, Debug, Serialize)]
pub struct PostTag {
    pub id: Uuid,
    pub name: String,
    pub display_name: String,
}

/// A post's tags, in the order they were typed.
///
/// The order is `position`, recorded when the tags were saved. It used to be
/// `created_at`, which is transaction time and therefore the same value for
/// every tag on a post — so the tags came back in whatever order the rows
/// happened to be read in, and the edit form handed them back reshuffled.
pub async fn get_tags_for_post(
    tx: &mut Transaction<'_, Postgres>,
    post_id: Uuid,
) -> Result<Vec<PostTag>> {
    let tags = sqlx::query_as!(
        PostTag,
        r#"
        SELECT h.id, h.name, h.display_name
        FROM tags h
        JOIN post_tags ph ON h.id = ph.tag_id
        WHERE ph.post_id = $1
        ORDER BY ph.position ASC, ph.created_at ASC, h.name ASC
        "#,
        post_id
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(tags)
}

/// One page of a tag's posts, plus how many there are in total for this viewer.
///
/// The total is counted over the same filtered set the page is taken from, so
/// the number at the top of the tag page is the number of drawings underneath
/// it — including the sensitive ones this viewer has chosen to see, and
/// excluding the ones they have not.
///
/// "Public" here is the predicate the home grid and profile pages use: a public
/// community, or no community at all. Personal posts used to be dropped by an
/// inner join, which left every tag that had only ever been used on one showing
/// an empty page from a link the post itself displayed.
pub async fn find_posts_by_tag(
    tx: &mut Transaction<'_, Postgres>,
    tag_name: &str,
    limit: i64,
    offset: i64,
    viewer_user_id: Option<Uuid>,
    viewer_show_sensitive: bool,
) -> Result<(Vec<crate::models::post::SerializablePostForHome>, i64)> {
    let rows = sqlx::query!(
        r#"
        SELECT
            posts.id,
            posts.title,
            posts.author_id,
            users.login_name,
            images.paint_duration,
            images.stroke_count,
            images.image_filename,
            images.width,
            images.height,
            images.replay_filename,
            posts.viewer_count,
            (posts.is_sensitive OR posts.is_explicit) AS "is_sensitive!",
            communities.slug AS "community_slug?",
            communities.name AS "community_name?",
            posts.published_at,
            posts.created_at,
            posts.updated_at,
            count(*) OVER () AS "total!"
        FROM posts
        JOIN post_tags ph ON posts.id = ph.post_id
        JOIN tags h ON ph.tag_id = h.id
        JOIN images ON posts.image_id = images.id
        JOIN users ON posts.author_id = users.id
        LEFT JOIN communities ON posts.community_id = communities.id
        WHERE h.name = $1
        AND posts.published_at IS NOT NULL
        AND posts.deleted_at IS NULL
        AND (communities.visibility = 'public' OR posts.community_id IS NULL)
        AND ((posts.is_sensitive = false AND posts.is_explicit = false) OR $4 = true OR posts.author_id = $5)
        ORDER BY posts.published_at DESC
        LIMIT $2 OFFSET $3
        "#,
        tag_name,
        limit,
        offset,
        viewer_show_sensitive,
        viewer_user_id
    )
    .fetch_all(&mut **tx)
    .await?;

    let total = rows.first().map(|row| row.total).unwrap_or(0);

    Ok((
        rows.into_iter()
            .map(|row| crate::models::post::SerializablePostForHome {
                id: row.id,
                title: row.title,
                author_id: row.author_id,
                user_login_name: row.login_name.into(),
                paint_duration: row.paint_duration.microseconds.to_string(),
                stroke_count: row.stroke_count,
                image_filename: row.image_filename,
                image_width: row.width,
                image_height: row.height,
                replay_filename: row.replay_filename,
                is_sensitive: row.is_sensitive,
                community_slug: row.community_slug,
                community_name: row.community_name,
                viewer_count: row.viewer_count,
                published_at: row.published_at,
                created_at: row.created_at,
                updated_at: row.updated_at,
            })
            .collect(),
        total,
    ))
}

/// Escape a user's search text so it matches literally.
///
/// `_` is a legal character in a tag and `%` is not, so without this, typing
/// `%` listed every tag on the site and `_` silently matched any character.
pub fn escape_like(query: &str) -> String {
    query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Tags matching what someone has typed, for the search box and the
/// autocomplete.
///
/// Substring rather than prefix — searching `cat` should find `black_cat` — with
/// the tags that start with the query first. Only tags with visible posts:
/// suggesting one with nothing behind it sends people to an empty page, and the
/// browse listings have always hidden those.
pub async fn search_tags(
    tx: &mut Transaction<'_, Postgres>,
    query: &str,
    limit: i64,
) -> Result<Vec<Tag>> {
    let escaped = escape_like(&normalize_tag(query));
    let tags = sqlx::query_as!(
        Tag,
        r#"
        SELECT
            h.id,
            h.name,
            h.display_name,
            s.post_count AS "post_count!",
            h.created_at,
            h.updated_at
        FROM tags h
        JOIN tag_stats s ON s.tag_id = h.id
        WHERE h.name LIKE '%' || $1 || '%' ESCAPE '\'
        ORDER BY (h.name LIKE $1 || '%' ESCAPE '\') DESC, s.post_count DESC, h.name ASC
        LIMIT $2
        "#,
        escaped,
        limit
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(tags)
}

/// How a browse listing is ordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagSort {
    Trending,
    Popular,
    Recent,
    Alphabetical,
}

impl TagSort {
    /// Anything unrecognised browses the default rather than 400ing a URL
    /// someone shared.
    pub fn from_param(sort: Option<&str>) -> Self {
        match sort {
            Some("popular") => Self::Popular,
            Some("recent") => Self::Recent,
            Some("alphabetical") => Self::Alphabetical,
            _ => Self::Trending,
        }
    }

    pub fn as_param(self) -> &'static str {
        match self {
            Self::Trending => "trending",
            Self::Popular => "popular",
            Self::Recent => "recent",
            Self::Alphabetical => "alphabetical",
        }
    }
}

/// Tags with posts, ordered as asked.
///
/// Trending is how many posts a tag has had in the last week, then its total,
/// then how recently it was used. The old score decayed on `tags.updated_at`
/// — a column touched only when the post counter changed, which included a post
/// being *deleted*, so removing a drawing made its tags trend.
pub async fn browse_tags(
    tx: &mut Transaction<'_, Postgres>,
    sort: TagSort,
    limit: i64,
) -> Result<Vec<Tag>> {
    let tags = sqlx::query_as!(
        Tag,
        r#"
        SELECT
            h.id,
            h.name,
            h.display_name,
            s.post_count AS "post_count!",
            h.created_at,
            h.updated_at
        FROM tags h
        JOIN tag_stats s ON s.tag_id = h.id
        ORDER BY
            CASE WHEN $1 = 'trending' THEN s.recent_post_count END DESC,
            CASE WHEN $1 = 'trending' THEN s.post_count END DESC,
            CASE WHEN $1 = 'trending' THEN s.last_posted_at END DESC,
            CASE WHEN $1 = 'popular' THEN s.post_count END DESC,
            CASE WHEN $1 = 'recent' THEN s.last_posted_at END DESC,
            h.name ASC
        LIMIT $2
        "#,
        sort.as_param(),
        limit
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(tags)
}

/// A drawing on a tag's card in the directory.
#[derive(Clone, Debug, Serialize)]
pub struct TagCover {
    pub tag_id: Uuid,
    pub image_filename: String,
    pub width: i32,
    pub height: i32,
}

/// The latest drawings behind each of `tag_ids`, up to three a tag, for
/// the directory's cards: a tag is a set of drawings, and its name alone
/// says little about them.
///
/// The tag page's rules for what shows -- published, not deleted, in a
/// public community or none -- and never a sensitive drawing: a cover is
/// seen before anyone has chosen to open anything, so it is not the viewer's
/// setting that decides but the drawing's.
pub async fn tag_covers(
    tx: &mut Transaction<'_, Postgres>,
    tag_ids: &[Uuid],
) -> Result<Vec<TagCover>> {
    if tag_ids.is_empty() {
        return Ok(Vec::new());
    }
    let covers = sqlx::query_as!(
        TagCover,
        r#"
        SELECT
            tag_id AS "tag_id!",
            image_filename AS "image_filename!",
            width AS "width!",
            height AS "height!"
        FROM (
            SELECT
                ph.tag_id,
                images.image_filename,
                images.width,
                images.height,
                row_number() OVER (
                    PARTITION BY ph.tag_id
                    ORDER BY posts.published_at DESC
                ) AS rank
            FROM post_tags ph
            JOIN posts ON posts.id = ph.post_id
            JOIN images ON images.id = posts.image_id
            LEFT JOIN communities ON communities.id = posts.community_id
            WHERE ph.tag_id = ANY($1)
            AND posts.published_at IS NOT NULL
            AND posts.deleted_at IS NULL
            AND (communities.visibility = 'public' OR posts.community_id IS NULL)
            AND posts.is_sensitive = false
            AND posts.is_explicit = false
        ) latest
        WHERE rank <= 3
        ORDER BY tag_id, rank
        "#,
        tag_ids
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(covers)
}

/// One tag by its normalized name. Tags with no visible posts still resolve:
/// the page says so rather than 404ing on a link a post is still displaying.
pub async fn find_tag_by_name(
    tx: &mut Transaction<'_, Postgres>,
    name: &str,
) -> Result<Option<Tag>> {
    let tag = sqlx::query_as!(
        Tag,
        r#"
        SELECT
            h.id,
            h.name,
            h.display_name,
            COALESCE(s.post_count, 0) AS "post_count!",
            h.created_at,
            h.updated_at
        FROM tags h
        LEFT JOIN tag_stats s ON s.tag_id = h.id
        WHERE h.name = $1
        "#,
        name
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(input: &str) -> Vec<String> {
        parse_tag_input(input)
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    #[test]
    fn splits_on_every_separator_a_reader_might_type() {
        // The field's hint says "commas or spaces". The placeholder is
        // translated into Japanese, Korean and Chinese, where an IME emits the
        // full-width forms of both, and every one of these used to arrive as a
        // single tag.
        assert_eq!(names("art, drawing"), vec!["art", "drawing"]);
        assert_eq!(names("art drawing"), vec!["art", "drawing"]);
        assert_eq!(names("art\tdrawing"), vec!["art", "drawing"]);
        assert_eq!(names("イラスト　お絵かき"), vec!["イラスト", "お絵かき"]);
        assert_eq!(names("그림、스케치"), vec!["그림", "스케치"]);
        assert_eq!(names("art，drawing"), vec!["art", "drawing"]);
    }

    #[test]
    fn the_hash_people_type_is_a_separator_not_a_character() {
        // `#art` used to be stored under the name `#art`: it rendered as
        // `##art` and linked to `/tags/#art`, which is a fragment, so the
        // link went to the tag directory instead of the tag.
        assert_eq!(names("#art"), vec!["art"]);
        assert_eq!(names("#art #drawing"), vec!["art", "drawing"]);
        assert_eq!(names("#art#drawing"), vec!["art", "drawing"]);
        assert_eq!(names("＃絵"), vec!["絵"]);
    }

    #[test]
    fn drops_characters_that_would_break_the_tags_own_link() {
        // `/tags/:name` is one path segment: a `/` in the name 404s, a `%`
        // is a malformed escape and 400s, and `?` and `#` truncate the URL.
        assert_eq!(names("a/b"), vec!["ab"]);
        assert_eq!(names("100%"), vec!["100"]);
        assert_eq!(names("what?"), vec!["what"]);
        assert_eq!(names("c++"), vec!["c"]);
        assert_eq!(names("🎨"), Vec::<String>::new());
    }

    #[test]
    fn keeps_the_scripts_the_site_is_actually_written_in() {
        assert_eq!(
            names("그림 イラスト рисунок 素描 art2"),
            vec!["그림", "イラスト", "рисунок", "素描", "art2"]
        );
    }

    #[test]
    fn folds_case_and_hyphens_but_shows_what_was_typed() {
        let parsed = parse_tag_input("Oekaki-Time");
        assert_eq!(
            parsed,
            vec![("oekaki_time".to_string(), "Oekaki_Time".to_string())]
        );
    }

    #[test]
    fn one_tag_twice_is_one_tag() {
        // The count used to be incremented once per parsed entry while the link
        // row was inserted once, so `Art art` was worth two posts.
        assert_eq!(names("Art art ART"), vec!["art"]);
    }

    #[test]
    fn caps_length_and_count_rather_than_failing_the_publish() {
        // `tags.name` is varchar(255): an over-long tag used to abort the
        // transaction the publish was running in, and the caller ignored the
        // error, so the reader got an unexplained 500 from a later query.
        let long = "a".repeat(400);
        assert_eq!(names(&long), vec!["a".repeat(MAX_TAG_LENGTH)]);

        let many = (0..MAX_TAGS_PER_POST + 10)
            .map(|i| format!("tag{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(names(&many).len(), MAX_TAGS_PER_POST);
    }

    #[test]
    fn empty_input_is_no_tags_not_one_empty_tag() {
        assert!(names("").is_empty());
        assert!(names("   ").is_empty());
        assert!(names(", , ,").is_empty());
        assert!(names("###").is_empty());
    }

    #[test]
    fn a_url_segment_normalizes_the_same_way_the_field_does() {
        // Otherwise /tags/Art and the link on a post tagged `Art` disagree.
        assert_eq!(normalize_tag("Oekaki-Time"), "oekaki_time");
        assert_eq!(normalize_tag("#art"), "art");
        assert_eq!(normalize_tag(&names("Oekaki-Time")[0]), "oekaki_time");
    }

    #[test]
    fn search_text_matches_literally() {
        // `%` used to be a wildcard the reader could type, listing everything.
        assert_eq!(escape_like("100%"), "100\\%");
        assert_eq!(escape_like("a_b"), "a\\_b");
        assert_eq!(escape_like("a\\b"), "a\\\\b");
    }
}
