use super::state::{AppState, Shutdown};
use crate::models::store_product;
use crate::models::user::Backend;
use crate::web::handlers::about::{about, design};
use crate::web::handlers::store::{do_app_store_notification, do_store_purchase, do_store_ticket};
use crate::web::handlers::supporter::supporter_page;
use crate::web::handlers::account::{
    account, delete_account_htmx, edit_account, edit_password, request_email_verification_code,
    save_language, save_show_sensitive_content, save_supporter_settings,
    verify_email_verification_code,
};
use crate::web::handlers::activitypub::{
    activitypub_get_community, activitypub_get_post, activitypub_get_user,
    activitypub_post_community_inbox, activitypub_post_shared_inbox,
    activitypub_post_user_followers, activitypub_post_user_inbox, activitypub_webfinger,
};
use crate::web::handlers::admin::{
    admin_banners, admin_banners_fragment, admin_collaborative_sessions, admin_communities,
    admin_community_posts, admin_flag_banner, admin_flag_post, admin_post_detail, admin_posts,
    admin_add_store_product, admin_posts_fragment, admin_set_store_product_on_sale,
    admin_set_store_product_details, admin_set_store_product_sale_window, admin_store,
    admin_user_posts, admin_users, collaborative_archive_manifest, collaborative_archive_tail,
    collaborative_session_details, collaborative_session_chat, collaborative_session_reference,
    record_collaborative_session_check,
    download_collaborative_archive, download_collaborative_diagnostics,
    replay_collaborative_session,
};
use crate::web::handlers::auth::{do_login, do_logout, do_signup, login, signup};
use crate::web::handlers::identity::{
    apple_callback, apple_sign_in, apple_start, cancel_pending_identity, do_apple_sign_in,
    do_google_sign_in, do_identity_welcome, do_steam_sign_in, do_unlink_identity, google_callback,
    google_sign_in, google_start, handoff_claim, handoff_done, handoff_start, identity_welcome,
    steam_app_only,
};
use crate::web::handlers::collaborate::{
    claim_session_preview, collaborate_lobby, collaborate_sessions_fragment,
    create_collaborative_session_form, get_auth_info, get_collaboration_meta,
    load_more_collaborative_posts, report_session_diagnostics, save_collaborative_session,
    serve_collaborative_app, serve_session_preview, upload_session_preview,
    websocket_collaborate_handler,
};
use crate::web::handlers::collaborate_cleanup::cleanup_collaborative_sessions;
use crate::web::handlers::community::{
    communities, communities_fragment, community, community_comments, community_iframe,
    create_community_form, do_accept_invitation, do_create_community, do_leave_community,
    do_reject_invitation, get_members, hx_delete_community, hx_do_edit_community, hx_edit_community,
    invite_user, load_more_community_comments, load_more_community_posts, members_page, redirect_community_to_unified,
    remove_member, retract_invitation,
};
use crate::web::handlers::devices::register_device_handler;
use crate::web::handlers::draw::{
    banner_draw_finish, draw_finish, start_banner_draw, start_draw, start_draw_get,
};
use crate::web::handlers::tag::{
    load_more_tag_comments, load_more_tag_posts, tag_autocomplete, tag_cards, tag_comments,
    tag_discovery, tag_view,
};
use crate::web::handlers::home::{
    do_delete_comment, following_comments_page, home, joined_comments_page,
    load_more_community_feed_posts, load_more_following_comments, load_more_joined_comments,
    load_more_public_posts, load_more_recent_comments, load_more_timeline_posts,
    my_communities_feed, my_timeline, recent_comments_page,
};
use crate::web::handlers::notifications::{
    delete_notification_handler, get_unread_notification_count, hx_mark_all_notifications_read,
    list_notifications, mark_notification_read, notifications_fragment,
};
use crate::web::handlers::password_reset::{
    password_reset_request, password_reset_request_page, password_reset_verify,
    password_reset_verify_page,
};
use crate::web::handlers::policy::policy;
use crate::web::handlers::post::{
    add_reaction, do_create_comment, do_post_edit_community, draft_posts, hx_delete_post,
    hx_do_edit_post, hx_edit_post, post_edit_community, post_publish, post_publish_form,
    post_reactions_detail, post_relay_view, post_relay_view_by_login_name,
    post_replay_view_by_login_name, post_view_by_login_name, redirect_post_to_login_name,
    remove_reaction,
};
use crate::web::handlers::privacy::privacy;
use crate::web::handlers::profile::{
    banner_management, do_activate_banner, do_add_link, do_delete_banner, do_delete_guestbook_entry,
    do_delete_link, do_follow_profile, do_move_link_down, do_move_link_up, do_reply_guestbook_entry,
    do_unfollow_profile, do_write_guestbook_entry, guestbook, load_more_profile_posts,
    load_more_profile_private_posts, profile_banners_iframe,
    profile_comments, profile_iframe, profile_or_community, profile_private, profile_settings,
};
use crate::web::handlers::report::{hx_report_post, hx_report_profile};
use crate::web::handlers::search::search_page;
use crate::web::handlers::well_known::{
    android_assetlinks, apple_app_site_association, robots_txt, sitemap_xml,
};
use crate::web::handlers::{handler_404, health};
use activitypub_federation::config::{FederationConfig, FederationMiddleware};
use anyhow::Result;
use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::http::{header, Response, StatusCode};
use axum::response::Redirect;
use axum::routing::{delete, get, post, put};
use axum::Router;
use axum_login::{login_required, AuthManagerLayerBuilder};
use axum_messages::MessagesManagerLayer;
use std::any::Any;
use std::net::SocketAddr;
use time::Duration;
use tokio::signal;
use tokio::task::AbortHandle;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_sessions::cookie::SameSite;
use tower_sessions::{session_store::ExpiredDeletion, Expiry, SessionManagerLayer};
use crate::web::session_store::PostgresStore;

/// Everything served straight off disk.
///
/// Its own function so that a test can ask the real thing for a file rather
/// than assembling a second copy of these mounts and asking that -- a copy
/// agrees with itself no matter what happens here.
fn static_router() -> Router {
    Router::new()
        .nest_service("/static/viewer", ServeDir::new("neo-cucumber/dist-viewer"))
        // The staff-only session replay viewer. Its assets are public, like
        // every other bundle; what it can read is not, because the endpoints
        // it fetches from are behind the admin extractor.
        .nest_service("/static/replay", ServeDir::new("neo-cucumber/dist-replay"))
        // tegaki is MIT, which asks that its notice travel with the copies.
        // Only css/js/lib are mounted and none of those files carry a header,
        // so without this the notice is in the repository but not reachable
        // from the thing being served.
        .route_service("/static/tegaki/LICENSE", ServeFile::new("tegaki/LICENSE"))
        .nest_service("/static/tegaki/css", ServeDir::new("tegaki/css"))
        .nest_service("/static/tegaki/js", ServeDir::new("tegaki/js"))
        .nest_service("/static/tegaki/lib", ServeDir::new("tegaki/lib"))
        .nest_service(
            "/collaborate/assets",
            ServeDir::new("neo-cucumber/dist/assets"),
        )
        .nest_service(
            "/static/neo-cucumber",
            ServeDir::new("neo-cucumber/dist-offline"),
        )
        .nest_service("/static", ServeDir::new("static"))
}

pub struct App {
    state: AppState,
}

impl App {
    pub async fn new(state: AppState) -> Result<Self, Box<dyn std::error::Error>> {
        sqlx::migrate!().run(&state.db_pool).await?;

        store_product::refresh_any_on_sale(&state.db_pool).await?;

        Ok(Self { state })
    }

    pub async fn serve(self) -> Result<(), Box<dyn std::error::Error>> {
        let authn_backend: Backend = Backend {
            db: self.state.db_pool.clone(),
        };

        let session_store = PostgresStore::new(self.state.db_pool.clone());

        // Not `continuously_delete_expired`: that returns on the first error,
        // so one pool timeout ended the sweep for the life of the process and
        // left the error waiting to panic the server on its way out at the
        // next deploy (OEEE-CAFE-7K). A missed sweep is caught by the next.
        let deletion_task = tokio::task::spawn({
            let session_store = session_store.clone();
            async move {
                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
                loop {
                    interval.tick().await;
                    if let Err(e) = session_store.delete_expired().await {
                        tracing::warn!("Failed to delete expired sessions: {}", e);
                    }
                }
            }
        });

        let cleanup_task = tokio::task::spawn(cleanup_collaborative_sessions(self.state.clone()));

        // Achievements Steam has not heard about yet. Nothing waits on it or
        // needs stopping cleanly: an unlock cut short is sent again by the
        // next process.
        if let Some(steam) = self.state.config.steam.clone() {
            tokio::task::spawn(crate::steam::sync_achievements(
                self.state.db_pool.clone(),
                steam.clone(),
            ));
            // Supporter standing, from what Steam says each account owns of
            // the catalogue's Steam products -- read each time round, so
            // one added at /admin/store is asked about without a restart.
            tokio::task::spawn(crate::steam::recheck_supporters(
                self.state.db_pool.clone(),
                steam,
            ));
        }

        // The same, for what the App Store says about the purchases the iOS
        // app has handed over: a refund takes the mark away within a day.
        if let Some(app_store) = self.state.config.app_store.clone() {
            tokio::task::spawn(crate::app_store::recheck_supporters(
                self.state.db_pool.clone(),
                app_store,
            ));
        }

        // And for the purchase tokens the Android app has handed over.
        if let Some(google_play) = self.state.config.google_play.clone() {
            tokio::task::spawn(crate::google_play::recheck_supporters(
                self.state.db_pool.clone(),
                google_play,
            ));
        }

        let session_layer = SessionManagerLayer::new(session_store)
            .with_secure(self.state.config.env == "production")
            .with_same_site(SameSite::Lax)
            .with_expiry(Expiry::OnInactivity(Duration::seconds(60 * 60 * 24 * 30)));

        let auth_layer = AuthManagerLayerBuilder::new(authn_backend, session_layer).build();

        let static_router = static_router();

        let protected_router = Router::new()
            .route("/following", get(my_timeline))
            .route("/joined", get(my_communities_feed))
            .route("/api/following/posts", get(load_more_timeline_posts))
            .route("/api/joined/posts", get(load_more_community_feed_posts))
            .route("/following/comments", get(following_comments_page))
            .route("/joined/comments", get(joined_comments_page))
            .route("/api/following/comments", get(load_more_following_comments))
            .route("/api/joined/comments", get(load_more_joined_comments))
            .route("/notifications", get(list_notifications))
            .route("/api/notifications/items", get(notifications_fragment))
            .route(
                "/notifications/unread-count",
                get(get_unread_notification_count),
            )
            .route(
                "/notifications/mark-all-read",
                post(hx_mark_all_notifications_read),
            )
            .route(
                "/notifications/{notification_id}/mark-read",
                post(mark_notification_read),
            )
            .route(
                "/notifications/{notification_id}",
                delete(delete_notification_handler),
            )
            .route("/account", get(account))
            .route("/account", post(edit_account))
            .route("/account/password", post(edit_password))
            .route("/account/language", post(save_language))
            .route("/account/credits", post(save_supporter_settings))
            .route(
                "/account/show-sensitive-content",
                post(save_show_sensitive_content),
            )
            .route(
                "/account/request-verify-email",
                post(request_email_verification_code),
            )
            .route(
                "/account/verify-email",
                post(verify_email_verification_code),
            )
            .route("/account/delete", delete(delete_account_htmx))
            .route(
                "/account/identities/{provider}/unlink",
                post(do_unlink_identity),
            )
            .route("/comments", post(do_create_comment))
            .route("/posts/{post_id}/reactions/add", post(add_reaction))
            .route("/posts/{post_id}/reactions/remove", post(remove_reaction))
            .route("/communities/new", get(create_community_form))
            .route("/communities/@{slug}/members", get(members_page))
            .route(
                "/communities/@{slug}/members/{user_id}",
                delete(remove_member),
            )
            .route("/communities/@{slug}/leave", post(do_leave_community))
            .route(
                "/communities/@{slug}/invitations/{invitation_id}",
                delete(retract_invitation),
            )
            .route("/communities/{id}/members", get(get_members))
            .route("/communities/{id}/invite", post(invite_user))
            .route("/communities/{id}/members/{user_id}", delete(remove_member))
            .route("/invitations/{id}/accept", post(do_accept_invitation))
            .route("/invitations/{id}/reject", post(do_reject_invitation))
            .route("/logout", post(do_logout))
            .route(
                "/draw/finish",
                post(draw_finish).layer(DefaultBodyLimit::max(10 * 1024 * 1024)),
            )
            .route("/posts/publish", post(post_publish))
            .route("/posts/{id}/edit", get(hx_edit_post))
            .route("/posts/{id}/relay", get(post_relay_view))
            .route("/posts/{id}", put(hx_do_edit_post))
            .route("/posts/{id}", delete(hx_delete_post))
            .route("/@{login_name}/{id}/edit/community", get(post_edit_community))
            .route(
                "/@{login_name}/{id}/edit/community",
                post(do_post_edit_community),
            )
            .route("/banners/draw", get(start_banner_draw))
            .route("/banners/draw/finish", post(banner_draw_finish))
            .route("/posts/{id}/publish", get(post_publish_form))
            .route("/@{login_name}/follow", post(do_follow_profile))
            .route("/@{login_name}/unfollow", post(do_unfollow_profile))
            .route("/@{login_name}/guestbook", post(do_write_guestbook_entry))
            .route(
                "/@{login_name}/guestbook/{entry_id}",
                delete(do_delete_guestbook_entry),
            )
            .route(
                "/@{login_name}/guestbook/{entry_id}/reply",
                post(do_reply_guestbook_entry),
            )
            // The push token the app handed the page (app_bridge.jinja).
            .route("/devices", post(register_device_handler))
            // Staff-only. These live inside the login-gated router so signed-out
            // visitors are redirected to /login; the AdminUser extractor on each
            // handler is what rejects signed-in non-admins with a 403.
            .route("/admin", get(|| async { Redirect::to("/admin/posts") }))
            .route("/admin/posts", get(admin_posts))
            .route("/admin/posts-fragment", get(admin_posts_fragment))
            .route("/admin/posts/{post_id}", get(admin_post_detail))
            .route("/admin/posts/{post_id}/explicit", post(admin_flag_post))
            .route("/admin/users", get(admin_users))
            .route("/admin/users/{login_name}/posts", get(admin_user_posts))
            .route("/admin/communities", get(admin_communities))
            .route("/admin/communities/{slug}/posts", get(admin_community_posts))
            .route(
                "/admin/collaborative-sessions",
                get(admin_collaborative_sessions),
            )
            .route(
                "/admin/collaborative-sessions/{uuid}/archive",
                get(download_collaborative_archive),
            )
            .route(
                "/admin/collaborative-sessions/{uuid}/archive/tail",
                get(collaborative_archive_tail),
            )
            .route(
                "/admin/collaborative-sessions/{uuid}/details",
                get(collaborative_session_details),
            )
            .route(
                "/admin/collaborative-sessions/{uuid}/reference",
                get(collaborative_session_reference),
            )
            .route(
                "/admin/collaborative-sessions/{uuid}/check",
                post(record_collaborative_session_check),
            )
            .route(
                "/admin/collaborative-sessions/{uuid}/diagnostics",
                get(download_collaborative_diagnostics),
            )
            .route(
                "/admin/collaborative-sessions/{uuid}/manifest",
                get(collaborative_archive_manifest),
            )
            .route(
                "/admin/collaborative-sessions/{uuid}/chat",
                get(collaborative_session_chat),
            )
            .route(
                "/admin/collaborative-sessions/{uuid}",
                get(replay_collaborative_session),
            )
            // Where the inspector used to live, kept so links already pasted
            // somewhere still open it.
            .route(
                "/admin/collaborative-sessions/{uuid}/replay",
                get(replay_collaborative_session),
            )
            .route("/admin/banners", get(admin_banners))
            .route("/admin/banners-fragment", get(admin_banners_fragment))
            .route(
                "/admin/banners/{banner_id}/explicit",
                post(admin_flag_banner),
            )
            .route("/admin/store", get(admin_store).post(admin_add_store_product))
            .route(
                "/admin/store/{store}/{product}/on-sale",
                post(admin_set_store_product_on_sale),
            )
            .route(
                "/admin/store/{store}/{product}/sale-window",
                post(admin_set_store_product_sale_window),
            )
            .route(
                "/admin/store/{store}/{product}/details",
                post(admin_set_store_product_details),
            )
            .route_layer(login_required!(Backend, login_url = "/login"));

        let state = self.state.clone();
        let domain = state.config.domain.clone();
        let activitypub_data = FederationConfig::builder()
            .domain(domain)
            .app_data(state)
            .build()
            .await?;

        let activitypub_router = Router::new()
            .route("/.well-known/webfinger", get(activitypub_webfinger))
            .route("/ap/users/{login_name}", get(activitypub_get_user))
            .route("/ap/posts/{post_id}", get(activitypub_get_post))
            .route(
                "/ap/communities/{community_id}",
                get(activitypub_get_community),
            )
            .route(
                "/ap/users/{login_name}/inbox",
                post(activitypub_post_user_inbox),
            )
            .route(
                "/ap/users/{login_name}/followers",
                get(activitypub_post_user_followers),
            )
            .route(
                "/ap/communities/{community_id}/inbox",
                post(activitypub_post_community_inbox),
            )
            .route("/ap/inbox", post(activitypub_post_shared_inbox))
            .layer(FederationMiddleware::new(activitypub_data));

        let app = Router::new()
            .route("/", get(home))
            // Open to guests. A guest's drawing is kept on the device, and the
            // drafts page is where it waits for them to have an account;
            // uploading it (/draw/finish) is still for someone signed in.
            .route("/draw", get(start_draw_get))
            .route("/draw", post(start_draw))
            .route("/posts/drafts", get(draft_posts))
            .route("/health", get(health))
            .route("/robots.txt", get(robots_txt))
            .route("/sitemap.xml", get(sitemap_xml))
            .route(
                "/.well-known/apple-app-site-association",
                get(apple_app_site_association),
            )
            .route("/.well-known/assetlinks.json", get(android_assetlinks))
            .route("/api/home/posts", get(load_more_public_posts))
            // GET beside the protected router's POST (creating a comment);
            // axum merges the two methods on the one path.
            .route("/comments", get(recent_comments_page))
            .route("/api/home/comments", get(load_more_recent_comments))
            .route(
                "/api/communities/@{slug}/comments",
                get(load_more_community_comments),
            )
            .route(
                "/api/profiles/@{login_name}/posts",
                get(load_more_profile_posts),
            )
            .route(
                "/api/profiles/@{login_name}/private/posts",
                get(load_more_profile_private_posts),
            )
            .route("/api/collaborate/posts", get(load_more_collaborative_posts))
            .route("/api/communities/cards", get(communities_fragment))
            .route(
                "/api/communities/@{slug}/posts",
                get(load_more_community_posts),
            )
            // The site's own report modals. Same work, HTML back.
            .route("/posts/{post_id}/report", post(hx_report_post))
            .route("/@{login_name}/report", post(hx_report_profile))
            // Answers for itself when nobody is signed in, like the rest of
            // this router, rather than sending the button to /login.
            .route("/comments/{comment_id}", delete(do_delete_comment))
            .route("/communities", get(communities))
            .route("/communities", post(do_create_community))
            .route("/communities/@{slug}", get(redirect_community_to_unified))
            .route("/communities/{id}", get(community))
            .route("/communities/{id}", put(hx_do_edit_community))
            .route("/communities/{id}/delete", delete(hx_delete_community))
            .route("/communities/{id}/edit", get(hx_edit_community))
            .route("/communities/{id}/comments", get(community_comments))
            .route("/communities/{id}/embed", get(community_iframe))
            .route("/search", get(search_page))
            .route("/tags", get(tag_discovery))
            .route("/tags/{tag_name}", get(tag_view))
            .route("/tags/{tag_name}/posts", get(load_more_tag_posts))
            .route("/tags/{tag_name}/comments", get(tag_comments))
            .route("/api/tags/{tag_name}/comments", get(load_more_tag_comments))
            .route("/api/tags/autocomplete", get(tag_autocomplete))
            .route("/api/tags/cards", get(tag_cards))
            .route("/@{slug}", get(profile_or_community))
            .route("/@{login_name}/embed", get(profile_iframe))
            .route("/@{login_name}/banners/embed", get(profile_banners_iframe))
            .route("/@{login_name}/settings/links", post(do_add_link))
            .route("/@{login_name}/settings/links/{id}", delete(do_delete_link))
            .route("/@{login_name}/settings/links/{id}/up", post(do_move_link_up))
            .route(
                "/@{login_name}/settings/links/{id}/down",
                post(do_move_link_down),
            )
            .route("/@{login_name}/settings", get(profile_settings))
            .route("/@{login_name}/settings/banners", get(banner_management))
            .route("/banners/{banner_id}/activate", post(do_activate_banner))
            .route("/banners/{banner_id}", delete(do_delete_banner))
            .route("/@{login_name}/guestbook", get(guestbook))
            .route("/@{login_name}/comments", get(profile_comments))
            .route("/@{login_name}/private", get(profile_private))
            .route("/@{login_name}/{post_id}", get(post_view_by_login_name))
            .route(
                "/@{login_name}/{post_id}/reactions",
                get(post_reactions_detail),
            )
            .route(
                "/@{login_name}/{post_id}/replay",
                get(post_replay_view_by_login_name),
            )
            .route(
                "/@{login_name}/{post_id}/relay",
                get(post_relay_view_by_login_name),
            )
            .route("/posts/{id}", get(redirect_post_to_login_name))
            .route(
                "/collaborate",
                get(collaborate_lobby).post(create_collaborative_session_form),
            )
            // Static segment, so it wins over /collaborate/:uuid — and no UUID
            // can spell "sessions" anyway.
            .route("/collaborate/sessions", get(collaborate_sessions_fragment))
            .route("/collaborate/", get(serve_collaborative_app))
            .route(
                "/collaborate/{uuid}",
                get(serve_collaborative_app)
                    .post(save_collaborative_session)
                    .layer(DefaultBodyLimit::max(
                        crate::web::handlers::collaborate::http_handlers::MAX_SAVE_BYTES,
                    )),
            )
            .route("/collaborate/{uuid}/ws", get(websocket_collaborate_handler))
            // What the room's canvas looks like right now, rendered by a
            // participant's browser because the server cannot draw. See
            // handlers/collaborate/preview.rs.
            .route(
                "/collaborate/{uuid}/preview",
                get(serve_session_preview)
                    .put(upload_session_preview)
                    .layer(DefaultBodyLimit::max(
                        crate::web::handlers::collaborate::preview::MAX_PREVIEW_BYTES,
                    )),
            )
            .route(
                "/collaborate/{uuid}/preview/claim",
                post(claim_session_preview),
            )
            // What a client believed about its own position when something
            // told it otherwise. See handlers/collaborate/archive.rs.
            .route(
                "/collaborate/{uuid}/diagnostics",
                post(report_session_diagnostics).layer(DefaultBodyLimit::max(
                    crate::web::handlers::collaborate::archive::MAX_DIAGNOSTIC_BYTES,
                )),
            )
            .route("/api/auth", get(get_auth_info))
            .route("/collaboration/{uuid}/meta", get(get_collaboration_meta))
            .route("/about", get(about))
            .route("/supporter", get(supporter_page))
            // What a store sold, handed over by the page in an app that
            // sells through it. See handlers/store.rs.
            .route("/store/{store}/purchases", post(do_store_purchase))
            .route("/store/apple/notifications", post(do_app_store_notification))
            .route("/store/{store}/tickets", post(do_store_ticket))
            .route("/design", get(design))
            .route("/privacy", get(privacy))
            .route("/policy", get(policy))
            .route("/signup", get(signup))
            .route("/signup", post(do_signup))
            .route("/login", get(login))
            .route("/login", post(do_login))
            .route("/auth/steam/app", get(steam_app_only))
            .route("/auth/steam", post(do_steam_sign_in))
            .route("/auth/apple", get(apple_sign_in))
            .route("/auth/apple", post(do_apple_sign_in))
            .route("/auth/apple/callback", post(apple_callback))
            .route("/auth/apple/start", post(apple_start))
            .route("/auth/google", get(google_sign_in))
            .route("/auth/google", post(do_google_sign_in))
            .route("/auth/google/callback", get(google_callback))
            .route("/auth/google/start", post(google_start))
            .route("/auth/handoff/start", post(handoff_start))
            .route("/auth/handoff/claim", post(handoff_claim))
            .route("/auth/handoff/done", get(handoff_done))
            .route("/auth/welcome", get(identity_welcome))
            .route("/auth/welcome", post(do_identity_welcome))
            .route("/auth/cancel", post(cancel_pending_identity))
            .route("/language", post(crate::web::language::set_language))
            .route("/password-reset", get(password_reset_request_page))
            .route("/password-reset", post(password_reset_request))
            .route("/password-reset/verify", get(password_reset_verify_page))
            .route("/password-reset/verify", post(password_reset_verify))
            .fallback(handler_404)
            .merge(protected_router)
            // Inside the auth layer, so the session is already in the
            // extensions and a failed request can be explained in the
            // language the reader chose rather than the one they asked for.
            .layer(axum::middleware::from_fn(crate::web::htmx::error_banner))
            // Outside the banner, which reads the header this rewrites: a
            // signed out reader's chosen language, standing in for their
            // browser's (web/language.rs).
            .layer(axum::middleware::from_fn(
                crate::web::language::language_cookie,
            ))
            .layer(MessagesManagerLayer)
            .layer(auth_layer)
            .with_state(self.state.clone())
            .merge(static_router)
            .merge(activitypub_router)
            // Outermost, so it also covers panics raised inside the layers
            // above. Without this axum drops the connection on a panic: the
            // client sees a reset with no status, and Sentry never hears about
            // it.
            .layer(CatchPanicLayer::custom(handle_panic));

        // run our app with hyper, listening globally
        let addr = SocketAddr::from(([0, 0, 0, 0], self.state.config.port));
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .expect("Failed to bind TCP listener");
        tracing::info!("listening on {}", addr);

        // Ensure we use a shutdown signal to abort the background tasks.
        axum::serve(listener, app.into_make_service())
            .with_graceful_shutdown(shutdown_signal(
                deletion_task.abort_handle(),
                cleanup_task.abort_handle(),
                self.state.shutdown.clone(),
            ))
            .await?;

        // Axum considers a WebSocket done as soon as it is upgraded, so it is
        // on us to wait for the drawing sessions to say goodbye and record
        // that their participants left.
        drain_websockets(&self.state).await;

        // Both tasks were aborted by the shutdown signal above, so a
        // cancellation here is the expected outcome rather than a failure —
        // treating it as one turned every SIGTERM into a panic on the way out.
        match deletion_task.await {
            Ok(()) => {}
            Err(e) if e.is_cancelled() => {}
            Err(e) => return Err(e.into()),
        }
        match cleanup_task.await {
            Ok(()) => {}
            Err(e) if e.is_cancelled() => {}
            Err(e) => return Err(e.into()),
        }

        Ok(())
    }
}

// Long enough for open sessions to close cleanly, short enough to stay inside
// the container's stop grace period (see `stop_grace_period` in
// docker-compose.yml) so the wait never ends in a SIGKILL instead.
const WEBSOCKET_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const WEBSOCKET_DRAIN_POLL: std::time::Duration = std::time::Duration::from_millis(50);

async fn drain_websockets(state: &AppState) {
    let live = state.shutdown.live_socket_count();
    if live == 0 {
        return;
    }

    tracing::info!("waiting for {} websocket session(s) to close", live);
    let deadline = tokio::time::Instant::now() + WEBSOCKET_DRAIN_TIMEOUT;
    while state.shutdown.live_socket_count() > 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(WEBSOCKET_DRAIN_POLL).await;
    }

    match state.shutdown.live_socket_count() {
        0 => tracing::info!("all websocket sessions closed"),
        remaining => tracing::warn!(
            "giving up on {} websocket session(s) after {:?}",
            remaining,
            WEBSOCKET_DRAIN_TIMEOUT
        ),
    }
}

/// Turn a panicking handler into a logged, reported 500 instead of a dropped
/// connection.
fn handle_panic(err: Box<dyn Any + Send + 'static>) -> Response<Body> {
    let details = if let Some(s) = err.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = err.downcast_ref::<&str>() {
        (*s).to_string()
    } else {
        "unknown panic payload".to_string()
    };

    tracing::error!("handler panicked: {}", details);
    sentry::capture_message(
        &format!("handler panicked: {}", details),
        sentry::Level::Fatal,
    );

    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"code":"INTERNAL_ERROR","message":"Something went wrong"}"#,
        ))
        .expect("panic response is statically valid")
}

async fn shutdown_signal(
    deletion_task_abort_handle: AbortHandle,
    cleanup_task_abort_handle: AbortHandle,
    shutdown: Shutdown,
) {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutdown signal received");
    deletion_task_abort_handle.abort();
    cleanup_task_abort_handle.abort();
    // Tell live drawing sessions to wind down now, in parallel with axum
    // draining the plain HTTP connections.
    shutdown.signal();
}

