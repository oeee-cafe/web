//! The list of communities, and searching it.

use crate::app_error::AppError;
use crate::models::community::{
    get_communities_members_count, get_own_communities, get_participating_communities,
    get_public_communities, get_public_communities_paginated, search_public_communities,
    CommunitySort,
};
use crate::models::post::find_recent_posts_by_communities;
use crate::models::user::AuthSession;
use crate::web::state::AppState;
use axum::extract::Query;
use axum::{extract::State, response::Html};
use axum_messages::Messages;
use minijinja::context;
use serde::Deserialize;
use uuid::Uuid;

use crate::web::context::CommonContext;
use crate::web::i18n::ExtractFtlLang;
use minijinja::value::Serde;

/// Communities per batch in the public directory.
const COMMUNITIES_PER_BATCH: i64 = 20;

/// Attaches the per-community extras the cards render: three recent posts and
/// the contributor count. Shared by the directory page and the infinite-scroll
/// fragment so a card looks the same however it arrived.
async fn enrich_public_communities(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    communities: &[crate::models::community::PublicCommunity],
    viewer_user_id: Option<Uuid>,
    viewer_show_sensitive: bool,
) -> Result<Vec<serde_json::Value>, AppError> {
    if communities.is_empty() {
        return Ok(Vec::new());
    }

    let ids: Vec<Uuid> = communities.iter().map(|c| c.id).collect();
    let recent_posts =
        find_recent_posts_by_communities(tx, &ids, 3, viewer_user_id, viewer_show_sensitive)
            .await?;
    let members_stats = get_communities_members_count(tx, &ids).await?;

    let mut posts_by: std::collections::HashMap<Uuid, Vec<serde_json::Value>> =
        std::collections::HashMap::new();
    // The query hands back each community's posts newest first, so the first one
    // seen is the last time the community was active — the key the default sort
    // orders by, which the card had no way to show.
    let mut last_post_by: std::collections::HashMap<Uuid, chrono::DateTime<chrono::Utc>> =
        std::collections::HashMap::new();
    for post in recent_posts {
        if let Some(community_id) = post.community_id {
            if let Some(published_at) = post.published_at {
                last_post_by.entry(community_id).or_insert(published_at);
            }
            posts_by
                .entry(community_id)
                .or_default()
                .push(serde_json::json!({
                    "id": post.id.to_string(),
                    "image_filename": post.image_filename,
                    "image_width": post.image_width,
                    "image_height": post.image_height,
                    "author_login_name": post.author_login_name,
                }));
        }
    }

    let mut members_by: std::collections::HashMap<Uuid, Option<i64>> =
        std::collections::HashMap::new();
    for stat in members_stats {
        members_by.insert(stat.community_id, stat.members_count);
    }

    Ok(communities
        .iter()
        .map(|community| {
            serde_json::json!({
                "id": community.id.to_string(),
                "name": community.name,
                "slug": community.slug,
                "description": community.description,
                "visibility": community.visibility,
                "owner_login_name": community.owner_login_name,
                "posts_count": community.posts_count,
                "members_count": members_by.get(&community.id).cloned().unwrap_or(None),
                "recent_posts": posts_by.get(&community.id).cloned().unwrap_or_default(),
                "last_post_at": last_post_by.get(&community.id),
            })
        })
        .collect())
}

/// GET /api/communities/cards — one batch of public community cards plus the
/// next sentinel, for htmx to swap in.
pub async fn communities_fragment(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Query(query): Query<CommunitiesQuery>,
) -> Result<Html<String>, AppError> {
    let offset = query.offset.unwrap_or(0).max(0);

    let (viewer_user_id, viewer_show_sensitive) = match auth_session.user.as_ref() {
        Some(user) => (Some(user.id), user.show_sensitive_content),
        None => (None, false),
    };

    let term = query.q.as_deref().map(str::trim).filter(|s| !s.is_empty());

    let mut tx = state.db_pool.begin().await?;
    let rows = match term {
        Some(term) => {
            search_public_communities(&mut tx, term, COMMUNITIES_PER_BATCH, offset).await?
        }
        None => {
            get_public_communities_paginated(&mut tx, query.sort, COMMUNITIES_PER_BATCH, offset)
                .await?
        }
    };
    let has_more = rows.len() as i64 == COMMUNITIES_PER_BATCH;
    let communities =
        enrich_public_communities(&mut tx, &rows, viewer_user_id, viewer_show_sensitive).await?;
    tx.commit().await?;

    let rendered = state.render("community_cards_fragment.jinja", context! {
        communities => Serde(communities),
        has_more => has_more,
        next_url => communities_fragment_url(query.sort, term, offset + COMMUNITIES_PER_BATCH),
        r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
    }).await?;

    Ok(Html(rendered))
}

#[derive(Debug, Deserialize)]
pub struct CommunitiesQuery {
    /// Missing or unrecognised sorts fall back to last-active.
    #[serde(default)]
    pub sort: CommunitySort,
    /// Row offset for the infinite-scroll sentinel. The first page omits it.
    pub offset: Option<i64>,
    /// Search term. Server-side because the directory is paginated now — a
    /// client-side filter would only ever search the batches already loaded.
    pub q: Option<String>,
}

/// URL the infinite-scroll sentinel fetches next. Built in Rust so the search
/// term gets percent-encoded.
fn communities_fragment_url(sort: CommunitySort, q: Option<&str>, next_offset: i64) -> String {
    let mut url = format!(
        "/api/communities/cards?offset={}&sort={}",
        next_offset,
        sort.as_param()
    );
    if let Some(term) = q.filter(|s| !s.trim().is_empty()) {
        url.push_str(&format!("&q={}", urlencoding::encode(term)));
    }
    url
}

pub async fn communities(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Query(query): Query<CommunitiesQuery>,
    messages: Messages,
) -> Result<Html<String>, AppError> {
    let sort = query.sort;
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Fetch all communities
    let own_communities_raw = match auth_session.user.clone() {
        Some(user) => get_own_communities(&mut tx, user.id).await?,
        None => vec![],
    };

    // The official section must show every official community regardless of
    // which page it would land on, so it is picked from the full list. That
    // query carries no per-community enrichment; the expensive part is bounded
    // below by what actually gets rendered.
    let official_raw: Vec<_> = get_public_communities(&mut tx)
        .await?
        .into_iter()
        .filter(|c| c.owner_login_name == state.config.official_account_login_name)
        .collect();

    let public_communities_raw =
        get_public_communities_paginated(&mut tx, sort, COMMUNITIES_PER_BATCH, 0).await?;
    let public_has_more = public_communities_raw.len() as i64 == COMMUNITIES_PER_BATCH;

    let participating_communities_raw = match auth_session.user.clone() {
        Some(user) => get_participating_communities(&mut tx, user.id).await?,
        None => vec![],
    };

    // Collect all community IDs for batch queries
    let mut all_community_ids: Vec<Uuid> = Vec::new();
    all_community_ids.extend(own_communities_raw.iter().map(|c| c.id));
    all_community_ids.extend(participating_communities_raw.iter().map(|c| c.id));
    all_community_ids.sort();
    all_community_ids.dedup();

    let (viewer_user_id, viewer_show_sensitive) = if let Some(ref user) = auth_session.user {
        (Some(user.id), user.show_sensitive_content)
    } else {
        (None, false)
    };

    // Fetch recent posts (3 per community) for all communities
    let recent_posts = find_recent_posts_by_communities(
        &mut tx,
        &all_community_ids,
        3,
        viewer_user_id,
        viewer_show_sensitive,
    )
    .await?;

    // Fetch members count (unique contributors) and posts count for all communities
    let members_stats = get_communities_members_count(&mut tx, &all_community_ids).await?;

    let community_stats = if !all_community_ids.is_empty() {
        sqlx::query!(
            r#"
            SELECT
                p.community_id,
                COUNT(p.id) as posts_count
            FROM posts p
            WHERE p.community_id = ANY($1)
                AND p.published_at IS NOT NULL
                AND p.deleted_at IS NULL
            GROUP BY p.community_id
            "#,
            &all_community_ids
        )
        .fetch_all(&mut *tx)
        .await?
    } else {
        Vec::new()
    };

    // Fetch owner login names for own and participating communities
    let owner_ids: Vec<Uuid> = own_communities_raw
        .iter()
        .chain(participating_communities_raw.iter())
        .map(|c| c.owner_id)
        .collect();

    let owner_logins = if !owner_ids.is_empty() {
        sqlx::query!(
            r#"
            SELECT id, login_name
            FROM users
            WHERE id = ANY($1)
            "#,
            &owner_ids
        )
        .fetch_all(&mut *tx)
        .await?
    } else {
        Vec::new()
    };

    // Group posts by community_id
    use std::collections::HashMap as StdHashMap;
    let mut posts_by_community: StdHashMap<Uuid, Vec<serde_json::Value>> = StdHashMap::new();
    // Newest first per community, so the first one seen is the community's last
    // activity. Same trick as enrich_public_communities.
    let mut last_post_by_community: StdHashMap<Uuid, chrono::DateTime<chrono::Utc>> =
        StdHashMap::new();
    for post in recent_posts {
        if let Some(community_id) = post.community_id {
            if let Some(published_at) = post.published_at {
                last_post_by_community
                    .entry(community_id)
                    .or_insert(published_at);
            }
            let posts = posts_by_community.entry(community_id).or_default();
            posts.push(serde_json::json!({
                "id": post.id.to_string(),
                "image_filename": post.image_filename,
                "image_width": post.image_width,
                "image_height": post.image_height,
                "author_login_name": post.author_login_name,
            }));
        }
    }

    // Create stats lookup maps
    let mut members_by_community: StdHashMap<Uuid, Option<i64>> = StdHashMap::new();
    for stat in members_stats {
        members_by_community.insert(stat.community_id, stat.members_count);
    }

    let mut posts_count_by_community: StdHashMap<Uuid, Option<i64>> = StdHashMap::new();
    for stat in community_stats {
        if let Some(community_id) = stat.community_id {
            posts_count_by_community.insert(community_id, stat.posts_count);
        }
    }

    // Create owner login lookup map
    let mut owner_login_by_id: StdHashMap<Uuid, String> = StdHashMap::new();
    for owner in owner_logins {
        owner_login_by_id.insert(owner.id, owner.login_name);
    }

    // One "yours" band rather than two: get_own_communities matches on ownership
    // and get_participating_communities on membership, and an owner is normally
    // also a member, so the two lists overlapped and rendered the same community
    // twice before you reached anything new. Deduped by id, owned first, then
    // ordered by last activity so the band leads with whatever is alive.
    let mut seen_your_ids: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    let mut your_communities_sorted: Vec<(
        Option<chrono::DateTime<chrono::Utc>>,
        serde_json::Value,
    )> = own_communities_raw
        .into_iter()
        .chain(participating_communities_raw)
        .filter(|community| seen_your_ids.insert(community.id))
        .map(|community| {
            let recent_posts = posts_by_community
                .get(&community.id)
                .cloned()
                .unwrap_or_default();
            let members_count = members_by_community
                .get(&community.id)
                .cloned()
                .unwrap_or(None);
            let posts_count = posts_count_by_community
                .get(&community.id)
                .cloned()
                .unwrap_or(None);
            let owner_login_name = owner_login_by_id
                .get(&community.owner_id)
                .cloned()
                .unwrap_or_default();
            let last_post_at = last_post_by_community.get(&community.id).copied();

            let card = serde_json::json!({
                "id": community.id.to_string(),
                "name": community.name,
                "slug": community.slug,
                "description": community.description,
                "visibility": community.visibility,
                "owner_login_name": owner_login_name,
                "posts_count": posts_count,
                "members_count": members_count,
                "recent_posts": recent_posts,
                "last_post_at": last_post_at,
            });
            (last_post_at, card)
        })
        .collect();
    // Descending, so a community nobody has drawn in sorts last rather than
    // first — which is where a plain sort on Option would put None.
    your_communities_sorted.sort_by_key(|(last_post_at, _)| std::cmp::Reverse(*last_post_at));
    let your_communities: Vec<serde_json::Value> = your_communities_sorted
        .into_iter()
        .map(|(_, card)| card)
        .collect();

    let public_communities = enrich_public_communities(
        &mut tx,
        &public_communities_raw,
        viewer_user_id,
        viewer_show_sensitive,
    )
    .await?;

    let official_communities = enrich_public_communities(
        &mut tx,
        &official_raw,
        viewer_user_id,
        viewer_show_sensitive,
    )
    .await?;

    let common_ctx = CommonContext::build(&mut tx, auth_session.user.as_ref(), &ftl_lang).await?;

    tx.commit().await?;

    let template = "communities.jinja";
    let rendered = state
        .render_page(
            template,
            common_ctx,
            context! {
                messages => Serde(messages.into_iter().collect::<Vec<_>>()),
                sort => sort.as_param(),
                // Same key names the fragment uses, so the first batch and every
                // scrolled batch render through one template.
                has_more => public_has_more,
                next_url => communities_fragment_url(sort, None, COMMUNITIES_PER_BATCH),
                official_communities => Serde(official_communities),
                // Key name must match community_cards_fragment.jinja's loop variable;
                // the page includes that template with this context.
                communities => Serde(public_communities),
                your_communities => Serde(your_communities),
                r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
            },
        )
        .await?;

    Ok(Html(rendered))
}

#[cfg(test)]
mod tests {
    use super::communities_fragment_url;
    use crate::models::community::CommunitySort;
    use crate::web::handlers::test_support;
    use minijinja::context;
    use minijinja::value::Serde;
    use serde_json::json;

    fn sample_community() -> serde_json::Value {
        json!({
            "id": "00000000-0000-0000-0000-000000000003",
            "name": "Open Studio",
            "slug": "open",
            "description": "a place",
            "visibility": "public",
            "owner_login_name": "someone",
            "posts_count": 12,
            "members_count": 4,
            "recent_posts": [{
                "id": "00000000-0000-0000-0000-000000000001",
                "image_filename": "abcdef.png",
                "image_width": 300,
                "image_height": 300,
                "author_login_name": "someone",
            }],
            "last_post_at": "2026-01-02T03:04:05Z",
        })
    }

    fn directory_context(
        your_communities: Vec<serde_json::Value>,
        current_user: serde_json::Value,
    ) -> minijinja::Value {
        context! {
            current_user => Serde(current_user),
            messages => Serde(Vec::<serde_json::Value>::new()),
            your_communities => Serde(your_communities),
            official_communities => Serde(Vec::<serde_json::Value>::new()),
            communities => Serde(vec![sample_community()]),
            sort => "active",
            has_more => true,
            next_url => "/api/communities/cards?offset=20&sort=active",
            draft_post_count => 0,
            unread_notification_count => 0,
            ftl_lang => "en",
        }
    }

    #[test]
    fn sentinel_url_round_trips_sort_and_search() {
        // The sentinel has to carry both, or scrolling a sorted or searched
        // directory silently reverts to the default listing.
        let url = communities_fragment_url(CommunitySort::Posts, None, 20);
        assert_eq!(url, "/api/communities/cards?offset=20&sort=posts");

        let url = communities_fragment_url(CommunitySort::Name, Some("art club"), 40);
        assert_eq!(
            url,
            "/api/communities/cards?offset=40&sort=name&q=art%20club"
        );
    }

    #[test]
    fn blank_search_is_not_carried_into_the_sentinel() {
        let url = communities_fragment_url(CommunitySort::Active, Some("   "), 20);
        assert_eq!(url, "/api/communities/cards?offset=20&sort=active");
    }

    #[test]
    fn directory_page_renders_its_first_batch() {
        // Regression: the page passed `public_communities` while the included
        // fragment looped over `communities`, so the first batch silently
        // rendered empty and the sentinel skipped straight to offset=20.
        let env = test_support::env();
        let template = env
            .get_template("communities.jinja")
            .expect("template loads");
        let rendered = template
            .render(directory_context(Vec::new(), json!(null)))
            .expect("communities.jinja renders");
        assert!(
            rendered.contains("class=\"community-card\""),
            "first batch did not render inside the page"
        );
        assert!(rendered.contains("Open Studio"));
        assert!(rendered.contains("infinite-scroll-sentinel"));
        // The directory is a grid in the wide container, like the home feed.
        // At .center it fit exactly one community per row.
        assert!(rendered.contains("class=\"center-wide communities-page\""));
        assert!(rendered.contains("class=\"community-grid\""));
    }

    #[test]
    fn card_shows_last_activity_and_hides_the_slug() {
        // The directory defaults to sorting on last activity, so the card has
        // to show it. The slug does not appear: it links where the name already
        // links, and for most communities it is a raw UUID.
        let env = test_support::env();
        let template = env
            .get_template("communities.jinja")
            .expect("template loads");
        let rendered = template
            .render(directory_context(Vec::new(), json!(null)))
            .expect("renders");
        assert!(rendered.contains("2026-01-02"), "last activity not shown");
        assert!(
            !rendered.contains("&gt;@open&lt;") && !rendered.contains(">@open<"),
            "slug rendered as its own line again"
        );
        // The owner handle is still credited.
        assert!(rendered.contains("@someone"));
    }

    #[test]
    fn a_community_with_no_posts_still_renders_a_card() {
        // Owned-but-empty communities appear in the "yours" band, where they
        // have no thumbnails and no last-activity date. Both used to be the
        // only things on the card with any height.
        let env = test_support::env();
        let template = env
            .get_template("communities.jinja")
            .expect("template loads");
        let mut empty = sample_community();
        empty["name"] = json!("Nothing Yet");
        empty["recent_posts"] = json!([]);
        empty["posts_count"] = json!(0);
        empty["last_post_at"] = json!(null);
        let rendered = template
            .render(directory_context(
                vec![empty],
                json!({"login_name": "someone"}),
            ))
            .expect("renders");
        assert!(rendered.contains("Nothing Yet"));
        assert!(rendered.contains("community-card-empty"));
    }

    #[test]
    fn yours_is_a_view_beside_the_directory() {
        let env = test_support::env();
        let template = env
            .get_template("communities.jinja")
            .expect("template loads");
        let rendered = template
            .render(directory_context(
                vec![sample_community()],
                json!({"login_name": "someone"}),
            ))
            .expect("renders");
        // One view at a time: the public directory shows first and the
        // others wait behind their tabs.
        assert!(rendered.contains("data-communities-tab=\"yours\""));
        assert!(rendered.contains("data-communities-panel=\"yours\" hidden"));
        assert!(rendered.contains("data-communities-panel=\"public\">"));
        // One list, not two: the participating section was a second copy of
        // every community you both own and are a member of.
        assert!(!rendered.contains("participating-community"));
        // Signed out, there is no Yours and nothing to create.
        let rendered = template
            .render(directory_context(Vec::new(), json!(null)))
            .expect("renders");
        assert!(!rendered.contains("data-communities-tab=\"yours\""));
        assert!(!rendered.contains("/communities/new"));
    }

    #[test]
    fn search_belongs_to_the_list_it_actually_filters() {
        // Its hx-target has always been the public list alone, so it is shown
        // with that list and hidden with it.
        let env = test_support::env();
        let template = env
            .get_template("communities.jinja")
            .expect("template loads");
        let rendered = template
            .render(directory_context(
                vec![sample_community()],
                json!({"login_name": "someone"}),
            ))
            .expect("renders");
        let filters = rendered
            .find("data-communities-for=\"public\"")
            .expect("filters");
        let search = rendered
            .find("id=\"community-search\"")
            .expect("search box");
        let sort = rendered.find("name=\"sort\"").expect("sort");
        let panel = rendered.find("data-communities-panel=").expect("panels");
        assert!(filters < search && search < sort && sort < panel);
    }

    #[test]
    fn renders_community_cards_fragment_standalone() {
        let env = test_support::env();
        let template = env
            .get_template("community_cards_fragment.jinja")
            .expect("template loads");
        let rendered = template
            .render(context! {
                communities => Serde(vec![sample_community()]),
                has_more => true,
                next_url => "/api/communities/cards?offset=20&sort=posts",
                r2_public_endpoint_url => "https://example.test",
            })
            .expect("renders standalone");
        assert!(rendered.contains(r#"href="/@open""#));
        assert!(rendered.contains("hx-trigger=\"revealed\""));
    }

    #[test]
    fn fragment_shows_empty_state_and_no_sentinel() {
        let env = test_support::env();
        let template = env
            .get_template("community_cards_fragment.jinja")
            .expect("template loads");
        let rendered = template
            .render(context! {
                communities => Serde(Vec::<serde_json::Value>::new()),
                has_more => false,
                next_url => "",
                r2_public_endpoint_url => "https://example.test",
            })
            .expect("renders");
        assert!(!rendered.contains("infinite-scroll-sentinel"));
        assert!(rendered.contains("active-communities-nil"));
    }
}
