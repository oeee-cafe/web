//! The account page, and being a supporter.

use crate::web::handlers::test_support;
use minijinja::context;
use serde_json::json;

use super::chrome;
use minijinja::value::Serde;

/// Only a supporter is asked whether to be in the credits, and the box
/// says what they chose.
#[test]
fn a_supporter_chooses_whether_to_be_credited() {
    let render = |show_in_credits: serde_json::Value| {
        render_account(show_in_credits, json!(["steam"]), json!("steam"))
    };
    let squash = |html: String| html.split_whitespace().collect::<Vec<_>>().join(" ");
    let listed = squash(render(json!(true)));
    assert!(listed.contains(r#"action="/account/credits""#));
    assert!(listed.contains(r#"value="on" checked"#));
    let hidden = squash(render(json!(false)));
    assert!(hidden.contains(r#"action="/account/credits""#));
    assert!(!hidden.contains(r#"name="show_in_credits" id="show_in_credits" value="on" checked"#));
    assert!(!render(json!(null)).contains("/account/credits"));
}

/// The account page as the handler renders it, for a supporter who
/// bought this year's pack on `supporter_platforms` and wears
/// `worn_mark`.
fn render_account(
    show_in_credits: serde_json::Value,
    supporter_platforms: serde_json::Value,
    worn_mark: serde_json::Value,
) -> String {
    test_support::env()
        .get_template("account.jinja")
        .expect("account loads")
        .render(context! {
            current_user => Serde(json!({
                "id": "b95e3d1e-5a25-4d0a-9d3a-3a0b0a9b1c2d",
                "login_name": "oeee",
                "display_name": "오이",
                "email": null,
                "email_verified_at": null,
                "created_at": "2026-09-22T00:00:00Z",
                "preferred_language": null,
                "show_sensitive_content": false,
                "role": "user",
            })),
            languages => vec![("ko", "한국어"), ("en", "English")],
            identities => Serde(json!([{"provider": "steam", "display_hint": "오이", "subject": "76561197960287930"}])),
            has_password => true,
            show_in_credits => Serde(show_in_credits),
            supporter_platforms => Serde(supporter_platforms),
            worn_mark => Serde(worn_mark),
            steam_enabled => true,
            steam_linked => true,
            ..chrome()
        })
        .expect("account renders")
}

/// The Supporter Pack's page: what there is to press depends on who is
/// reading and on which store their app sells through. The handler
/// chooses the buttons (handlers/supporter.rs) -- the reader's store's
/// products on sale this year -- and the page draws exactly those, with
/// Restore only for the App Store.
#[test]
fn the_supporter_page_offers_what_there_is_to_buy() {
    let env = test_support::env();
    let render = |current_user: serde_json::Value,
                  store: serde_json::Value,
                  offers: serde_json::Value,
                  nothing_this_year: bool,
                  supporter_standings: serde_json::Value| {
        env.get_template("supporter.jinja")
            .expect("supporter loads")
            .render(context! {
                this_year => 2026,
                restorable => store == json!("apple"),
                store => Serde(store),
                offers => Serde(offers),
                nothing_this_year,
                supports_this_year => supporter_standings
                    .as_array()
                    .is_some_and(|standings| standings.iter().any(|s| s["year"] == json!(2026))),
                supporter_standings => Serde(supporter_standings),
                worn_mark => Serde(json!("steam")),
                ..context! { current_user => Serde(current_user), ..chrome() }
            })
            .expect("supporter renders")
    };
    let signed_in = json!({"login_name": "oeee", "display_name": "오이"});
    let none = json!(null);
    let pack = |product: &str, label: Option<&str>| json!({"product": product, "label": label});

    // Signed out: somewhere to sign in, and nothing to buy with.
    let out = render(none.clone(), json!("apple"), json!([]), false, json!([]));
    assert!(out.contains(r#"href="/login?next=/supporter""#));
    // The buttons themselves, not the script that listens for them.
    assert!(!out.contains(r#"data-product="#));
    assert!(!out.contains(r#"class="ds-button supporter-restore""#));

    // In the App Store: a button per product, in its own words where it
    // has some, and Restore.
    let apple = render(
        signed_in.clone(),
        json!("apple"),
        json!([
            pack("cafe.oeee.supporter.2026", None),
            pack(
                "cafe.oeee.supporter.2026.more",
                Some("Support twice as much")
            ),
        ]),
        false,
        json!([]),
    );
    let squashed = apple.split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(
        apple
            .matches(r#"class="ds-button ds-button-primary supporter-buy""#)
            .count(),
        2
    );
    assert!(squashed
        .contains(r#"data-product="cafe.oeee.supporter.2026">supporter-pack-buy(year=2026)<span"#));
    assert!(squashed
        .contains(r#"data-product="cafe.oeee.supporter.2026.more">Support twice as much<span"#));
    assert!(apple.contains(r#"class="ds-button supporter-restore""#));
    // Room for the price the app will fill in, keyed by the product it
    // belongs to, and empty until then.
    assert!(squashed.contains(
        r#"<span class="supporter-price" data-product="cafe.oeee.supporter.2026"></span>"#
    ));
    assert!(apple.contains("(app.store = app.store || {}).prices = "));
    assert!(!apple.contains("supporter-pack-none"));

    // On Steam, and in the Microsoft Store: no Restore, which only the
    // App Store has.
    for store in ["steam", "microsoft"] {
        let page = render(
            signed_in.clone(),
            json!(store),
            json!([pack("481", None)]),
            false,
            json!([]),
        );
        assert!(page.contains(r#"data-product="481""#), "{store}");
        assert!(
            !page.contains(r#"class="ds-button supporter-restore""#),
            "{store}"
        );
    }

    // A browser: no store, no button, and the line saying where the
    // pack is sold.
    let browser = render(signed_in.clone(), none.clone(), json!([]), false, json!([]));
    assert!(!browser.contains(r#"data-product="#));
    assert!(!browser.contains(r#"class="ds-button supporter-restore""#));
    assert!(browser.contains("supporter-pack-elsewhere"));

    // Nothing for this year yet: the page says so instead.
    let nothing = render(
        signed_in.clone(),
        json!("steam"),
        json!([]),
        true,
        json!([]),
    );
    assert!(!nothing.contains(r#"data-product="#));
    assert!(nothing.contains("supporter-pack-none(year=2026)"));

    // Already bought, and the years before it: the handler offers
    // nothing more in the store it was bought in.
    let owned = render(
        signed_in,
        json!("apple"),
        json!([]),
        false,
        json!([
            {"store": "steam", "year": 2025, "since": "2025-03-02T00:00:00Z"},
            {"store": "apple", "year": 2026, "since": "2026-01-08T00:00:00Z"},
            {"store": "microsoft", "year": 2026, "since": "2026-02-08T00:00:00Z"},
        ]),
    );
    assert!(owned.contains("supporter-pack-have(year=2026)"));
    assert!(!owned.contains(r#"data-product="#));
    assert!(!owned.contains("supporter-pack-none"));
    assert_eq!(owned.matches("supporter-chip").count(), 3, "one per year");
    assert!(owned.contains("🎮</span>2025"));
    assert!(owned.contains("🍎</span>2026"));
    assert!(owned.contains("🛍️</span>2026"));
}

/// Which platform's mark to wear is only a question for someone who
/// supports on more than one, and the answer they gave is the one
/// selected.
#[test]
fn only_a_supporter_on_two_platforms_is_asked_which_mark_to_wear() {
    let one = render_account(json!(true), json!(["steam"]), json!("steam"));
    assert!(!one.contains(r#"name="mark""#), "nothing to choose between");

    let both = render_account(json!(true), json!(["steam", "apple"]), json!("apple"));
    let squashed = both.split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(
        squashed.matches(r#"name="mark""#).count(),
        2,
        "one for each"
    );
    assert!(squashed.contains(r#"name="mark" value="apple" checked"#));
    assert!(!squashed.contains(r#"name="mark" value="steam" checked"#));
    // Saved by the same button as the credits, in the same form.
    assert_eq!(squashed.matches(r#"action="/account/credits""#).count(), 1);

    // Nobody else is asked at all.
    assert!(!render_account(json!(null), json!([]), json!(null)).contains(r#"name="mark""#));
}

/// An account made with Steam has no password: it is offered one to set
/// rather than asked for its current one, and deleting it asks for its
/// handle.
#[test]
fn the_account_page_asks_what_the_account_can_answer() {
    let env = test_support::env();
    let render = |has_password: bool, identities: serde_json::Value| {
        env.get_template("account.jinja")
            .expect("account loads")
            .render(context! {
                current_user => Serde(json!({
                    "id": "b95e3d1e-5a25-4d0a-9d3a-3a0b0a9b1c2d",
                    "login_name": "oeee",
                    "display_name": "오이",
                    "email": null,
                    "email_verified_at": null,
                    "created_at": "2026-09-22T00:00:00Z",
                    "preferred_language": null,
                    "show_sensitive_content": false,
                    "role": "user",
                })),
                languages => vec![("ko", "한국어"), ("en", "English")],
                identities => Serde(identities),
                has_password,
                steam_enabled => true,
                steam_linked => false,
                apple_enabled => true,
                apple_linked => false,
                google_enabled => true,
                google_linked => false,
                discord_enabled => true,
                discord_linked => false,
                messages => Serde(Vec::<serde_json::Value>::new()),
                draft_post_count => 0,
                unread_notification_count => 0,
                ftl_lang => "en",
            })
            .expect("account renders")
    };

    let with_password = render(true, json!([]));
    assert!(with_password.contains(r#"name="current_password""#));
    assert!(with_password.contains(r#"name="password""#));
    assert!(!with_password.contains(r#"id="delete_login_name""#));
    assert!(with_password.contains("account-linked-accounts-none"));
    assert!(with_password.contains("/auth/steam?next=/account"));
    assert!(with_password.contains("/auth/apple?next=/account"));
    assert!(with_password.contains("/auth/google?next=/account"));
    assert!(with_password.contains("/auth/discord?next=/account"));

    let without = render(
        false,
        json!([{"provider": "steam", "display_hint": "오이", "subject": "76561197960287930"}]),
    );
    assert!(!without.contains(r#"name="current_password""#));
    assert!(without.contains("account-set-password"));
    assert!(without.contains(r#"id="delete_login_name""#));
    assert!(without.contains("account-delete-type-login-name(loginName=oeee)"));
    assert!(without.contains(r#"action="/account/identities/steam/unlink""#));
    assert!(without.contains("Steam: 오이"));

    // Apple gives a name only the first time; the address stands in.
    let apple = render(
        true,
        json!([{"provider": "apple", "display_hint": null, "email": "x@privaterelay.appleid.com", "subject": "001234.abc"}]),
    );
    assert!(apple.contains("Apple: x@privaterelay.appleid.com"));

    // Apple and Google lead with the address even when they gave a name
    // too.
    let apple_named = render(
        true,
        json!([{"provider": "apple", "display_hint": "오이", "email": "x@privaterelay.appleid.com", "subject": "001234.abc"}]),
    );
    assert!(apple_named.contains("Apple: x@privaterelay.appleid.com"));
    assert!(!apple_named.contains("Apple: 오이"));
    let google = render(
        true,
        json!([{"provider": "google", "display_hint": "오이", "email": "oeee@example.test", "subject": "1234"}]),
    );
    assert!(google.contains("Google: oeee@example.test"));
    assert!(!google.contains("Google: 오이"));
    assert!(apple.contains(r#"action="/account/identities/apple/unlink""#));

    // Discord tells its accounts apart by name, not by address.
    let discord = render(
        true,
        json!([{"provider": "discord", "display_hint": "넬리", "email": "nelly@example.test", "subject": "80351110224678912"}]),
    );
    assert!(discord.contains("Discord: 넬리"));
    assert!(discord.contains(r#"action="/account/identities/discord/unlink""#));
}
