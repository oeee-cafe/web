//! A post's page: the drawing or its replay, its comments, its community.

use crate::web::handlers::test_support;
use minijinja::context;
use serde_json::json;

use super::{chrome, post_page, render_post_page};
use minijinja::value::Serde;

/// Posting a comment swaps the list for the one the server sends back.
/// The form used to sit inside the swapped element, so it went with it
/// and a second comment needed a reload; it has to sit outside it.
#[test]
fn the_comment_form_survives_posting_a_comment() {
    let author = "b95e3d1e-5a25-4d0a-9d3a-3a0b0a9b1c2d";
    let viewer = json!({"id": "0d2a2b4c-7e8f-4a1b-8c9d-1e2f3a4b5c6d", "role": "user", "login_name": "viewer"});
    let rendered = render_post_page("true", author, viewer);
    let list = rendered.find("id=\"comments\"").expect("comment list");
    let form = rendered.find("id=\"comment-form\"").expect("comment form");
    assert!(form > list, "the form comes after the list");
    let list_end = rendered[list..].find("</div>").map(|i| list + i).unwrap();
    assert!(form > list_end, "the form is inside the swapped list");
    assert!(rendered.contains("hx-target=\"#comments\" hx-swap=\"innerHTML\""));
}

/// A NEO drawing's replay controls are under it from the start, and the
/// drawing itself no longer leads to relaying it -- the Relay button does.
#[test]
fn a_replay_is_on_the_stage_and_the_drawing_is_not_a_link() {
    let author = "b95e3d1e-5a25-4d0a-9d3a-3a0b0a9b1c2d";
    let rendered = render_post_page("true", author, json!(null));
    assert!(rendered.contains("id=\"post-stage-replay\""));
    assert!(rendered.contains("data-poster="));
    let stage = rendered.find("class=\"post-stage\"").expect("stage");
    let side = rendered.find("class=\"post-side\"").expect("side");
    assert!(
        !rendered[stage..side].contains("/relay"),
        "the drawing links to relaying it again"
    );
}

/// Tegaki's replay has a page of its own, and it sits under the toolbar
/// like the painter it was drawn in, saying what the reader is doing.
#[test]
fn a_tegaki_replay_plays_under_the_toolbar() {
    let env = test_support::env();
    let mut post = super::replay_post();
    post["replay_filename"] = json!("abcdef0123.tgkr");
    let rendered = env
        .get_template("post_replay_view_tgkr.jinja")
        .expect("tegaki replay template loads")
        .render(context! {
            post => Serde(post),
            post_id => "9c881320-2b43-4afa-b2bb-7128c8a3e985",
            community_id => Serde(json!(null)),
            presence => Serde(json!({"activity": "watching_replay", "community": null, "group": null})),
            ..chrome()
        })
        .expect("tegaki replay renders");
    let nav = rendered.find("<nav class=\"nav-bar\"").expect("the toolbar");
    assert!(nav < rendered.find("Tegaki.open(").unwrap());
    assert!(rendered.find("tegaki.css").unwrap() < rendered.find("/static/ds.css").unwrap());
    assert!(rendered.contains(r#"<meta name="oeee-presence" content="watching_replay""#));
    assert!(rendered.contains("replayMode: true"));
    assert!(rendered.contains("abcdef0123.tgkr"));
    assert!(rendered.contains("fitUnderToolbar();"));
}

/// A post in a community is linked under the community's slug from its
/// own page -- its address, its share link and the pages beneath it --
/// and so is a drawing in its thread, from inside the thread-card macro,
/// while a reply outside any community keeps its author's name.
#[test]
fn a_post_in_a_community_is_linked_under_the_community() {
    let id = "9c881320-2b43-4afa-b2bb-7128c8a3e985";
    let mut post = post_page("true", "b95e3d1e-5a25-4d0a-9d3a-3a0b0a9b1c2d");
    post["community_slug"] = json!("club");
    post["community_name"] = json!("Club");
    let reply = |reply_id: &str, community_slug: serde_json::Value| {
        json!({
            "id": reply_id,
            "title": "A reply",
            "user_login_name": "replier",
            "user_display_name": "Replier",
            "image_filename": "abcdef0123.png",
            "comments_count": 0,
            "community_slug": community_slug,
            "children": [],
        })
    };
    let env = test_support::env();
    let rendered = env
        .get_template("post_view.jinja")
        .unwrap_or_else(|e| panic!("post_view.jinja loads: {e:#}"))
        .render(context! {
            post => Serde(post),
            post_id => id,
            current_user => Serde(json!({"id": "0d2a2b4c-7e8f-4a1b-8c9d-1e2f3a4b5c6d", "role": "user", "login_name": "viewer"})),
            r2_public_endpoint_url => "https://images.example",
            base_url => "https://oeee.example",
            domain => "oeee.example",
            comments => Serde(Vec::<serde_json::Value>::new()),
            collaborative_participants => Serde(Vec::<serde_json::Value>::new()),
            reaction_counts => Serde(Vec::<serde_json::Value>::new()),
            pickles => context! { count => 0, given_by_viewer => false, weekly_available => false },
            tags => Serde(Vec::<serde_json::Value>::new()),
            child_posts => Serde(vec![
                reply("00000000-0000-0000-0000-00000000000a", json!("club")),
                reply("00000000-0000-0000-0000-00000000000b", json!(null)),
            ]),
            post_community => Serde(json!(null)),
            parent_post_data => Serde(json!(null)),
            ..chrome()
        })
        .unwrap_or_else(|e| panic!("post_view.jinja renders: {e:#}"));

    assert!(rendered.contains(&format!(r#"content="https://oeee.example/@club/{id}""#)));
    assert!(rendered.contains(&format!(
        r#"data-share-url="https://oeee.example/@club/{id}""#
    )));
    assert!(rendered.contains(&format!(r#"href="/@club/{id}/relay""#)));
    assert!(rendered.contains(&format!(r#"href="/@club/{id}/reactions""#)));
    assert!(rendered.contains(r#"href="/@club/00000000-0000-0000-0000-00000000000a""#));
    assert!(rendered.contains(r#"href="/@replier/00000000-0000-0000-0000-00000000000b""#));
    assert!(
        !rendered.contains(&format!("/@someone/{id}")),
        "the post is linked under its author's name"
    );
}

/// The blur on sensitive drawings is one CSS rule, and a stylesheet
/// edit elsewhere once took it out with the rules around it: for a
/// day every sensitive drawing showed in the grids unblurred, and
/// nothing failed. The card puts the class on the image; this holds the
/// rule that makes it mean something.
#[test]
fn sensitive_drawings_stay_blurred() {
    let css = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("static/style.css"),
    )
    .expect("style.css reads");
    let rule = css
        .split(".posts-grid .posts-grid-item img.sensitive {")
        .nth(1)
        .and_then(|rest| rest.split('}').next())
        .expect("a .sensitive rule");
    assert!(
        rule.contains("filter: blur("),
        "the .sensitive rule no longer blurs"
    );
    let frame = css
        .split(".posts-grid .posts-grid-item > a:first-child {")
        .nth(1)
        .and_then(|rest| rest.split('}').next())
        .expect("the drawing's frame rule");
    assert!(
        frame.contains("overflow: hidden;"),
        "the frame no longer clips the blur, which spills onto the grid"
    );
    let card = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("templates/post_card.jinja"),
    )
    .expect("post_card.jinja reads");
    assert!(
        card.contains("sensitive{% endif %}"),
        "the card no longer marks sensitive drawings"
    );
}

/// The replay switch is enforced in the handler; this is the other half of
/// it -- the link a stranger is not supposed to be offered.
#[test]
fn a_closed_replay_is_linked_for_its_author_only() {
    let author = "b95e3d1e-5a25-4d0a-9d3a-3a0b0a9b1c2d";
    let stranger = json!({"id": "0d2a2b4c-7e8f-4a1b-8c9d-1e2f3a4b5c6d", "role": "user"});
    // A NEO replay is not linked but played under the drawing: what a
    // stranger must not be handed is the recording's address, which the
    // stage carries for the viewer.
    let link = "/replay/30/30ca3f590dda85e21dbc94250199a692b4fa5c7d626ea3445acef3bcf3c1338a.pch";

    let open = render_post_page("true", author, json!(null));
    assert!(
        open.contains(link),
        "an open replay should be linked for anyone"
    );

    let closed_to_stranger = render_post_page("false", author, stranger);
    assert!(
        !closed_to_stranger.contains(link),
        "a closed replay should not be linked for someone else"
    );

    let closed_to_author = render_post_page("false", author, json!({"id": author, "role": "user"}));
    assert!(
        closed_to_author.contains(link),
        "a closed replay should still be linked for its author"
    );
    assert!(
        closed_to_author.contains("(replay-private)"),
        "the author should be told the replay is only theirs to watch"
    );

    // Staff keep the link for moderation, under a label that does not tell
    // them the replay is theirs.
    let closed_to_staff = render_post_page(
        "false",
        author,
        json!({"id": "0d2a2b4c-7e8f-4a1b-8c9d-1e2f3a4b5c6d", "role": "admin"}),
    );
    assert!(
        closed_to_staff.contains(link),
        "a closed replay should still be linked for staff"
    );
    assert!(
        closed_to_staff.contains("(replay-private-staff)"),
        "staff should be told the replay is private, not that it is theirs"
    );
}

/// The edit form is where a published post's replay gets turned off, and
/// an unchecked box submits nothing -- so a box that fails to reflect the
/// stored value silently flips it on the next save.
#[test]
fn the_edit_form_reflects_the_stored_replay_switch() {
    let env = test_support::env();
    let template = env
        .get_template("post_edit.jinja")
        .unwrap_or_else(|e| panic!("post_edit.jinja loads: {e:#}"));
    let render = |allow_replay: &str| {
        template
            .render(context! {
                post => Serde(json!({
                    "title": "Tandemaus",
                    "content": "a description",
                    "is_sensitive": "false",
                    "allow_relay": "true",
                    "allow_replay": allow_replay,
                })),
                post_id => "9c881320-2b43-4afa-b2bb-7128c8a3e985",
                tags => "",
                ..chrome()
            })
            .unwrap_or_else(|e| panic!("post_edit.jinja renders: {e:#}"))
    };

    /// The rest of the `<input>` tag that carries the replay switch.
    fn checkbox(rendered: &str) -> String {
        let (_, tail) = rendered
            .rsplit_once("id=\"allow_replay\"")
            .expect("the replay checkbox");
        let (tag, _) = tail.split_once('>').expect("the checkbox tag ends");
        tag.to_string()
    }

    assert!(
        checkbox(&render("true")).contains("checked"),
        "an open replay should render a checked box"
    );
    assert!(
        !checkbox(&render("false")).contains("checked"),
        "a closed replay should render an unchecked box"
    );
}

/// The relay page is now rendered for personal posts too, where there is
/// no community to name above the canvas or to link back to. Every use of
/// one has to survive its absence.
#[test]
fn the_relay_page_renders_with_and_without_a_community() {
    let env = test_support::env();
    let template = env
        .get_template("draw_post_cucumber.jinja")
        .unwrap_or_else(|e| panic!("draw_post_cucumber.jinja loads: {e:#}"));
    let render = |community_name: serde_json::Value, community_slug: serde_json::Value| {
        template
            .render(context! {
                parent_post => Serde(json!({
                    "id": "9c881320-2b43-4afa-b2bb-7128c8a3e985",
                    "title": "Tandemaus",
                    "image_width": "640",
                    "image_height": "480",
                    "image_filename": "abcdef0123.png",
                    "login_name": "someone",
                })),
                width => 640,
                height => 480,
                community_name => Serde(community_name),
                community_slug => Serde(community_slug),
                community_id => Serde(json!(null)),
                is_relay => true,
                painter_config => "{}",
                ..chrome()
            })
            .unwrap_or_else(|e| panic!("draw_post_cucumber.jinja renders: {e:#}"))
    };

    // The painter has no bar of its own under the toolbar any more, so
    // the page title is what names the community a relay lands in.
    let title = |rendered: &str| {
        let start = rendered.find("<title>").expect("a title") + "<title>".len();
        let end = rendered.find("</title>").expect("a closed title");
        rendered[start..end]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let in_a_community = title(&render(json!("Tegaki"), json!("tegaki")));
    assert!(
        in_a_community.contains("Re: Tandemaus") && in_a_community.contains("@ Tegaki"),
        "a relay in a community should be titled with both, got: {in_a_community}"
    );

    let personal = title(&render(json!(null), json!(null)));
    assert!(personal.contains("Re: Tandemaus"), "got: {personal}");
    assert!(
        !personal.contains(" @ "),
        "a personal relay should name no community, got: {personal}"
    );
}

/// The author's own comment carries a delete button, and it asks the
/// site's route -- `/api/v1/comments/:id` went with the apps' JSON API.
#[test]
fn a_comment_deletes_itself_through_the_sites_own_route() {
    let env = test_support::env();
    let rendered = env
        .get_template("post_comments.jinja")
        .expect("post_comments loads")
        .render(context! {
            comments => Serde(json!([{
                "id": "0c8f0000-0000-0000-0000-000000000001",
                "actor_name": "Someone",
                "handle": {"login_name": "someone", "name": null, "host": null},
                "actor_url": "/@someone",
                "content": "hello",
                "content_html": null,
                "created_at": "2026-01-02T03:04:05Z",
                "deleted_at": null,
                "children": [],
            }])),
            current_user => Serde(json!({"login_name": "someone"})),
            supporters => Serde(json!({})),
            ftl_lang => "en",
        })
        .expect("post_comments renders");
    assert!(
        rendered.contains(r#"hx-delete="/comments/0c8f0000-0000-0000-0000-000000000001""#),
        "{rendered}"
    );
    assert!(!rendered.contains("/api/v1"));
}

/// A comment as `build_comment_thread_tree` serializes one.
fn comment(login_name: Option<&str>, name: &str) -> serde_json::Value {
    json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "post_id": "9c881320-2b43-4afa-b2bb-7128c8a3e985",
        "actor_id": uuid::Uuid::new_v4().to_string(),
        "parent_comment_id": null,
        "content": "hi",
        "content_html": "<p>hi</p>",
        "iri": null,
        "actor_name": name,
        "handle": match login_name {
            Some(login_name) => json!({"login_name": login_name, "name": null, "host": null}),
            None => json!({"login_name": null, "name": name, "host": "oeee.example"}),
        },
        "actor_url": "/@someone",
        "updated_at": "2026-09-22T00:00:00Z",
        "created_at": "2026-09-22T00:00:00Z",
        "deleted_at": null,
        "children": [],
    })
}

/// A supporter's mark goes beside every name on a post's page that
/// belongs to one -- the author, someone who drew with them, a
/// commenter, a reply -- and beside no one else's, remote accounts
/// included however they are named. Each wears their own platform's.
#[test]
fn supporters_wear_their_platforms_mark_on_a_post_page() {
    use std::collections::HashMap;
    let templates = crate::web::templates::Templates::new(test_support::env());
    let mut reply = comment(Some("fan"), "Fan");
    reply["children"] = json!([comment(Some("someone"), "Someone")]);
    reply["children"][0]["parent_comment_id"] = reply["id"].clone();
    let comments = json!([
        reply,
        comment(Some("plain"), "Plain"),
        // A remote account sharing a supporter's local name.
        comment(None, "fan"),
    ]);
    let page_context = || {
        context! {
            post => Serde(post_page("true", "b95e3d1e-5a25-4d0a-9d3a-3a0b0a9b1c2d")),
            post_id => "9c881320-2b43-4afa-b2bb-7128c8a3e985",
            r2_public_endpoint_url => "https://images.example",
            base_url => "https://oeee.example",
            domain => "oeee.example",
            comments => Serde(comments.clone()),
            collaborative_participants => Serde(json!([
                {"login_name": "someone", "display_name": "Someone"},
                {"login_name": "friend", "display_name": "Friend"},
            ])),
            reaction_counts => Serde(Vec::<serde_json::Value>::new()),
            pickles => context! { count => 0, given_by_viewer => false, weekly_available => false },
            tags => Serde(Vec::<serde_json::Value>::new()),
            child_posts => Serde(Vec::<serde_json::Value>::new()),
            post_community => Serde(json!(null)),
            parent_post_data => Serde(json!(null)),
            ..chrome()
        }
    };
    let supporting = |names: &[String]| -> HashMap<String, String> {
        [("someone", "steam"), ("friend", "apple"), ("fan", "steam")]
            .into_iter()
            .filter(|(name, _)| names.iter().any(|asked| asked == name))
            .map(|(name, mark)| (name.to_string(), mark.to_string()))
            .collect()
    };
    let badges = |html: &str| {
        html.matches(r#"class="ds-handle ds-handle-supporter ds-marked""#)
            .count()
    };

    let mut asked = Vec::new();
    let page = templates
        .render_with("post_view.jinja", page_context(), |names| {
            asked = names.to_vec();
            supporting(names)
        })
        .expect("post_view.jinja renders");
    // Everyone from here the page names, once each, and nobody else --
    // not the remote "fan", who has no login name to ask about.
    assert_eq!(asked, ["fan", "friend", "plain", "someone"]);
    // Author, co-drawer, the commenter and the author's reply to them.
    assert_eq!(badges(&page), 4);
    let byline = page.find("post-inspector-byline").unwrap();
    let handle = page[byline..].find("ds-handle").unwrap() + byline;
    assert!(
        page[handle..].starts_with("ds-handle ds-handle-supporter"),
        "the author's own pill"
    );
    // The co-drawer bought elsewhere and wears the other mark: one
    // storefront on the page, the rest gamepads.
    assert_eq!(page.matches(r#"title="supporter-badge-apple""#).count(), 1);
    assert_eq!(page.matches(r#"title="supporter-badge-steam""#).count(), 3);
    // No placeholder is left behind.
    assert!(!page.contains('\u{E000}'));

    let nobody = templates
        .render_with("post_view.jinja", page_context(), |_| HashMap::new())
        .unwrap();
    assert_eq!(badges(&nobody), 0);
    // The comments fragment an HTMX post swaps in, the same way.
    let fragment = templates
        .render_with(
            "post_comments.jinja",
            context! { comments => Serde(comments), ..chrome() },
            supporting,
        )
        .unwrap();
    // The commenter and the reply to them; the remote "fan" is not one.
    assert_eq!(badges(&fragment), 2);
}

/// A commenter from this site is @login_name, without the site's own
/// domain; one from elsewhere keeps the handle their server gave them,
/// with its host where nothing can hide it.
/// Name and handle's pill are printed with nothing between them, so the
/// gap is .ds-person's margin alone.
#[test]
fn a_comment_names_its_author_by_the_design_systems_person() {
    let env = test_support::env();
    let fragment = env
        .get_template("post_comments.jinja")
        .unwrap()
        .render(context! {
            comments => Serde(json!([comment(Some("plain"), "Plain"), comment(None, "far")])),
            ..chrome()
        })
        .unwrap();
    assert!(
        fragment.contains(r#"<a class="ds-handle" href="&#x2f;@plain">@plain</a>"#),
        "{fragment}"
    );
    // The host in a part of its own, which the name gives way to, and
    // the name in a <bdi> it cannot turn the host round from.
    // Each pill leads where its name does: the local one to the profile
    // here, the remote one to theirs, apart.
    assert!(fragment.contains(
        r#"<bdi class="ds-handle-name">@far</bdi><span class="ds-handle-host">@oeee.example</span></a>"#
    ));
    assert!(fragment.contains(r#"<a class="ds-handle ds-handle-remote" href="#));
    assert!(fragment.contains(r#"Plain</a><a class="ds-handle" href="#));
    // The remote author's profile is off the site, so it opens apart.
    assert!(fragment.contains(r#"target="_blank" rel="noopener noreferrer">far</a>"#));
}

#[test]
fn reactions_offer_any_emoji_and_quote_the_ones_they_show() {
    // The handler swaps this in with only these keys. A remote server can
    // react with any text at all, so the emoji goes into hx-vals as JSON.
    let env = test_support::env();
    let template = env
        .get_template("post_reactions.jinja")
        .unwrap_or_else(|e| panic!("post_reactions.jinja loads: {e:#}"));
    let reaction_counts = vec![
        context! { emoji => "👏", count => 2, reacted_by_user => true },
        context! { emoji => "a\"}'b", count => 1, reacted_by_user => false },
    ];
    let signed_in = template
        .render(context! {
            current_user => context! { id => "u" },
            reaction_counts => reaction_counts.clone(),
            post_id => "p",
            login_name => "someone",
            ftl_lang => "en",
        })
        .expect("reactions render signed in");
    assert!(
        signed_in.contains("reaction-custom-form"),
        "signed in, any emoji can be added"
    );
    assert!(
        signed_in.contains(r#"hx-vals='{"emoji": "a\"}\u0027b"}'"#),
        "a quote in an emoji must not close hx-vals or its attribute"
    );

    let signed_out = template
        .render(context! {
            reaction_counts => reaction_counts,
            post_id => "p",
            login_name => "someone",
            ftl_lang => "en",
        })
        .expect("reactions render signed out");
    assert!(!signed_out.contains("reaction-custom-form"));
}

/// The pickle chip, as the handler swaps it in. A supporter with this
/// week's to give gets a button; anyone else sees a count, or nothing at all
/// on a post with none; and nobody is offered their own post.
#[test]
fn a_pickle_is_offered_only_to_a_supporter_who_still_has_one() {
    let env = test_support::env();
    let template = env
        .get_template("post_pickles.jinja")
        .unwrap_or_else(|e| panic!("post_pickles.jinja loads: {e:#}"));
    let render = |viewer: Option<&str>, count: i64, given: bool, available: bool| {
        template
            .render(context! {
                current_user => viewer.map(|id| context! { id => id }),
                pickles => context! {
                    count => count,
                    given_by_viewer => given,
                    weekly_available => available,
                },
                post_id => "p",
                post_author_id => "author",
                ftl_lang => "en",
            })
            .expect("pickles render")
    };

    let offered = render(Some("supporter"), 2, false, true);
    assert!(offered.contains(r#"hx-post="/posts/p/pickles""#));
    // Two jars for the two it has, and the faded one the giver's will be.
    assert_eq!(offered.matches("<svg").count(), 3);
    assert!(offered.contains("pickle-slot"));
    // It says what it does in words, and tells a screen reader the count.
    assert!(offered.contains(r#"<span class="pickle-label">post-pickle-give-action</span>"#));
    assert!(offered.contains(r#"<span class="sr-only">, post-pickle-count(count=2)</span>"#));
    let first = render(Some("supporter"), 0, false, true);
    assert!(!first.contains("sr-only"), "{first}");
    assert!(offered.contains(r#"title="post-pickle-give""#));
    // Its confirmation names what yes does, and is not the red, destructive one.
    assert!(offered.contains(r#"data-confirm-action="post-pickle-give-action""#));
    assert!(offered.contains(r#"data-confirm-tone="plain""#));

    let own = render(Some("author"), 2, false, true);
    assert!(!own.contains("hx-post"), "nobody pickles their own post");
    assert!(own.contains("pickle-chip"));
    // The chip names itself, and pressing it says what a pickle is.
    assert!(own.contains(r#"<span class="pickle-label">post-pickle-count(count=2)</span>"#));
    assert!(own.contains("post-pickle-about"));
    assert!(own.contains(r#"href="/supporter""#));

    let given = render(Some("supporter"), 3, true, false);
    assert!(!given.contains("hx-post"));
    assert!(given.contains("pickle-chip given"));
    assert!(given.contains(r#"title="post-pickle-given""#));
    // The jars on the chip are the summary's; the panel has one of its own.
    let summary = |html: &str| {
        html.split("</summary>")
            .next()
            .unwrap()
            .matches("<svg")
            .count()
    };
    assert_eq!(summary(&given), 3);
    // Past five, one jar and the number.
    let many = render(Some("supporter"), 12, true, false);
    assert_eq!(summary(&many), 1);
    assert!(many.contains("post-pickle-count(count=12)"));
    // A pickle given to another post this week leaves this one to look at.
    let spent = render(Some("supporter"), 1, false, false);
    assert!(!spent.contains("hx-post"));

    let nothing = render(None, 0, false, false);
    // Empty to CSS's :empty, whitespace and all, so it takes no room.
    assert!(
        nothing.contains(r#"class="post-pickles"></div>"#),
        "{nothing}"
    );
}

/// The panel under a post's pickles says who gave them: each named giver
/// as a chip leading to their profile, the latest first, and the ones who
/// asked not to be named (as in the credits) as a count.
#[test]
fn a_posts_pickles_name_who_gave_them() {
    let env = test_support::env();
    let template = env
        .get_template("post_pickles.jinja")
        .unwrap_or_else(|e| panic!("post_pickles.jinja loads: {e:#}"));
    let render = |givers: serde_json::Value, unnamed: i64| {
        template
            .render(context! {
                current_user => context! { id => "viewer" },
                pickles => Serde(json!({
                    "count": givers.as_array().unwrap().len() as i64 + unnamed,
                    "given_by_viewer": false,
                    "weekly_available": false,
                    "givers": givers,
                    "unnamed": unnamed,
                })),
                post_id => "p",
                post_author_id => "author",
                ftl_lang => "en",
            })
            .expect("pickles render")
    };

    let both = render(
        json!([
            {"login_name": "mina", "display_name": "미나"},
            {"login_name": "jun", "display_name": "준"},
        ]),
        1,
    );
    assert!(both.contains("post-pickle-given-by"));
    let mina = both.find(r#"href="&#x2f;@mina""#).expect("mina is named");
    let jun = both.find(r#"href="&#x2f;@jun""#).expect("jun is named");
    assert!(mina < jun, "in the order given, the latest first");
    assert!(both.contains("post-pickle-unnamed(count=1)"));

    let named = render(json!([{"login_name": "mina", "display_name": "미나"}]), 0);
    assert!(!named.contains("post-pickle-unnamed"));

    let nobody_named = render(json!([]), 2);
    assert!(nobody_named.contains("post-pickle-unnamed(count=2)"));
    assert!(!nobody_named.contains(r#"href="&#x2f;@"#));
}

/// A guest who drew in the session is on the byline by the name they gave,
/// marked as a guest, with no profile link and no handle -- the name is
/// whatever they typed, and could be an account's.
#[test]
fn a_guest_on_a_collaborative_byline_is_named_but_not_linked() {
    let env = test_support::env();
    let rendered = env
        .get_template("post_view.jinja")
        .expect("post_view.jinja loads")
        .render(context! {
            post => Serde(post_page("true", "b95e3d1e-5a25-4d0a-9d3a-3a0b0a9b1c2d")),
            post_id => "9c881320-2b43-4afa-b2bb-7128c8a3e985",
            r2_public_endpoint_url => "https://images.example",
            base_url => "https://oeee.example",
            domain => "oeee.example",
            comments => Serde(Vec::<serde_json::Value>::new()),
            collaborative_participants => Serde(json!([
                {"login_name": "someone", "display_name": "Someone", "is_guest": false},
                {"login_name": "friend", "display_name": "Friend", "is_guest": false},
                {"login_name": "", "display_name": "impostor", "is_guest": true},
            ])),
            reaction_counts => Serde(Vec::<serde_json::Value>::new()),
            pickles => context! { count => 0, given_by_viewer => false, weekly_available => false },
            tags => Serde(Vec::<serde_json::Value>::new()),
            child_posts => Serde(Vec::<serde_json::Value>::new()),
            post_community => Serde(json!(null)),
            parent_post_data => Serde(json!(null)),
            ..chrome()
        })
        .expect("post_view.jinja renders");

    let byline = &rendered[rendered.find("post-inspector-byline").expect("a byline")..];
    let byline = &byline[..byline.find("post-edit-container").expect("the byline ends")];
    assert!(byline.contains("Friend"));
    assert!(rendered.contains(r#"<span class="ds-person-name">impostor</span>"#));
    assert!(rendered.contains(r#"<span class="ds-guest-badge">collaborate-guest-badge</span>"#));
    assert!(!rendered.contains(r#"href="/@""#), "a guest is not linked");
    assert!(!rendered.contains("@impostor"), "nor given a handle");
}

/// A card in a feed whose drawing was given a pickle has a jar before its
/// title, named in its tooltip and to a screen reader; one without has
/// none, and nor does the admin grid.
#[test]
fn a_pickled_drawing_has_a_jar_on_its_card() {
    let mut env = test_support::env();
    env.add_template_owned(
        "card_test.jinja",
        r#"{% import "post_card.jinja" as card %}{{ card.post_card(post, "https://example.test", admin) }}"#
            .to_string(),
    )
    .unwrap();
    let render = |pickled: bool, admin: bool| {
        env.get_template("card_test.jinja")
            .unwrap()
            .render(context! {
                post => Serde(json!({
                    "id": "p",
                    "title": "Cat",
                    "user_login_name": "artist",
                    "image_filename": "ab/cat.png",
                    "image_width": 300,
                    "image_height": 300,
                    "is_sensitive": false,
                    "published_at": "2026-10-14T03:00:00Z",
                    "pickled": pickled,
                })),
                admin => admin,
                ftl_lang => "en",
            })
            .expect("the card renders")
    };
    let pickled = render(true, false);
    assert!(
        pickled.contains(r#"aria-label="post-card-pickled""#),
        "{pickled}"
    );
    assert!(pickled.contains("pickle-icon"));
    // Before the title, inside its line.
    assert!(pickled.contains("</svg></span>Cat"), "{pickled}");
    assert!(!render(false, false).contains("post-card-pickled"));
    assert!(!render(true, true).contains("post-card-pickled"));
}
