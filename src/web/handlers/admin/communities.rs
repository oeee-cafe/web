//! Changing a community from the staff list.

use crate::app_error::AppError;
use crate::models::actor::Actor;
use crate::models::admin::{
    find_all_communities_with_activity, set_community_slug, AdminSort, SlugTaken,
};
use crate::models::community::CommunityVisibility;
use crate::web::handlers::AdminUser;
use crate::web::state::AppState;
use axum::extract::{Path, State};
use axum::response::Html;
use axum::Form;
use minijinja::context;
use minijinja::value::Serde;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct SetSlugForm {
    pub slug: String,
}

/// What `CK_communities_slug_RegularExpression` lets in.
fn is_valid_slug(slug: &str) -> bool {
    (1..=50).contains(&slug.len())
        && slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// POST /admin/communities/:community_id/slug — give a community a new slug,
/// which its owner could also do from its settings. Addressed by id, since
/// the slug is what changes. Answers with the list's row again, carrying
/// what went wrong if anything did.
pub async fn admin_set_community_slug(
    admin: AdminUser,
    State(state): State<AppState>,
    Path(community_id): Path<Uuid>,
    Form(form): Form<SetSlugForm>,
) -> Result<Html<String>, AppError> {
    let mut tx = state.db_pool.begin().await?;
    let find = |rows: Vec<_>| {
        rows.into_iter()
            .next()
            .ok_or_else(|| AppError::NotFound("Community".to_string()))
    };
    let community = find(
        find_all_communities_with_activity(&mut tx, AdminSort::default(), Some(community_id))
            .await?,
    )?;
    let slug = form.slug.trim();

    let error = if slug == community.slug {
        None
    } else if !is_valid_slug(slug) {
        Some("A slug is 1 to 50 letters, digits, _ or -.".to_string())
    } else {
        set_community_slug(&mut tx, community_id, slug, &state.config)
            .await?
            .err()
            .map(|taken| match taken {
                SlugTaken::Community => format!("Another community has @{slug}."),
                SlugTaken::Account => format!("@{slug} is an account's name."),
            })
    };

    let saved = error.is_none() && slug != community.slug;
    let community = if saved {
        let updated = find(
            find_all_communities_with_activity(&mut tx, AdminSort::default(), Some(community_id))
                .await?,
        )?;
        let actor = Actor::find_by_community_id(&mut tx, community_id).await?;
        tx.commit().await?;
        crate::staff_webhook::notify(crate::staff_webhook::Event::CommunitySlugSet {
            from: community.slug.clone(),
            to: updated.slug.clone(),
            by: admin.0.login_name.to_string(),
        });
        // Followers elsewhere learn the new handle the way an owner's edit
        // tells them (`update_community_with_activity`).
        if let Some(actor) = actor.filter(|_| {
            updated.deleted_at.is_none() && updated.visibility != CommunityVisibility::Private
        }) {
            if let Err(e) =
                crate::web::handlers::activitypub::send_update_activity(&actor, &state).await
            {
                tracing::warn!(
                    "Failed to send Update activity for community {}: {:?}",
                    community_id,
                    e
                );
            }
        }
        updated
    } else {
        community
    };

    let rendered = state
        .render(
            "admin/community_row.jinja",
            context! {
                community => Serde(community),
                // What was typed stays in the field when it was refused.
                slug => error.as_ref().map(|_| slug),
                saved,
                error,
            },
        )
        .await?;

    Ok(Html(rendered))
}

#[cfg(test)]
mod tests {
    use super::is_valid_slug;

    #[test]
    fn a_slug_is_what_the_column_allows() {
        assert!(is_valid_slug("oekaki_club-2"));
        assert!(is_valid_slug(&"a".repeat(50)));
        assert!(!is_valid_slug(""));
        assert!(!is_valid_slug(&"a".repeat(51)));
        assert!(!is_valid_slug("그림"));
        assert!(!is_valid_slug("a b"));
        assert!(!is_valid_slug("a.b"));
    }
}
