//! Signing up and signing in, and the providers offered for it.

use crate::web::handlers::test_support;
use minijinja::context;
use serde_json::json;

use super::chrome;
use minijinja::value::Serde;

/// Signing up asks for agreement to the two pages it links, and the box
/// is required in the page as the handler requires it on the server.
#[test]
fn signup_asks_for_agreement_to_the_guidelines_and_privacy_policy() {
    let env = test_support::env();
    let rendered = env
        .get_template("signup.jinja")
        .expect("signup loads")
        .render(context! {
            current_user => Serde(json!(null)),
            messages => Serde(Vec::<serde_json::Value>::new()),
            next => "/collaborate",
            ftl_lang => "en",
        })
        .expect("signup renders");
    assert!(rendered.contains(r#"<input type="checkbox" name="agree" value="1" required />"#));
    assert!(rendered.contains(r#"href="/policy""#) || rendered.contains("&#x2f;policy"));
    assert!(rendered.contains(r#"href="/privacy""#) || rendered.contains("&#x2f;privacy"));
    assert!(rendered.contains("signup-agree"));
    // Signing in instead keeps where the reader was going.
    assert!(rendered.contains("/login?next="));
}

/// Signing up after signing in with Steam: a handle and a name, the
/// agreement, no password -- and a way to sign into an existing account
/// instead, which keeps where the reader was going.
#[test]
fn the_welcome_page_asks_for_a_handle_and_the_agreement() {
    let env = test_support::env();
    let render = |provider_name: serde_json::Value, error: serde_json::Value| {
        env.get_template("identity_welcome.jinja")
            .expect("welcome loads")
            .render(context! {
                provider => "Steam",
                provider_name => Serde(provider_name),
                login_name => "",
                display_name => "오이",
                error => Serde(error),
                next => "/collaborate",
                ..chrome()
            })
            .expect("welcome renders")
    };

    let rendered = render(json!("오이"), json!(null));
    assert!(rendered.contains(r#"name="login_name""#));
    assert!(rendered.contains(r#"value="오이""#));
    assert!(rendered.contains(r#"<input type="checkbox" name="agree" value="1" required />"#));
    assert!(!rendered.contains(r#"type="password""#));
    assert!(rendered.contains("identity-welcome-body(name=오이,provider=Steam)"));
    assert!(rendered.contains("/login?next="));
    assert!(rendered.contains(r#"action="/auth/cancel""#));
    assert!(!rendered.contains("auth-error"));

    let rendered = render(json!(null), json!("This username is already taken."));
    assert!(rendered.contains("identity-welcome-body-unnamed(provider=Steam)"));
    assert!(rendered.contains("This username is already taken."));
}

#[test]
fn signing_in_offers_steam_only_where_it_is_on_and_says_what_it_will_link() {
    let env = test_support::env();
    let render = |steam_enabled: bool, linking_provider: serde_json::Value| {
        env.get_template("login.jinja")
            .expect("login loads")
            .render(context! {
                next => "/draw",
                steam_enabled,
                linking_provider => Serde(linking_provider),
                ..chrome()
            })
            .expect("login renders")
    };

    let off = render(false, json!(null));
    assert!(!off.contains(r#"href="/auth/steam"#));
    assert!(!off.contains("identity-login-notice"));

    let on = render(true, json!(null));
    assert!(on.contains("auth-steam"));
    assert!(on.contains("/auth/steam?next="));

    // Signing in to claim a Steam account: say so, offer a way out, and
    // do not offer Steam again.
    let linking = render(true, json!("Steam"));
    assert!(linking.contains("identity-login-notice(provider=Steam)"));
    assert!(linking.contains(r#"action="/auth/cancel""#));
    assert!(!linking.contains(r#"href="/auth/steam"#));
}

#[test]
fn steam_is_offered_just_above_discord() {
    let env = test_support::env();
    let page = env
        .get_template("login.jinja")
        .expect("login loads")
        .render(context! {
            next => "/draw",
            steam_enabled => true,
            apple_enabled => true,
            google_enabled => true,
            discord_enabled => true,
            linking_provider => Serde(json!(null)),
            ..chrome()
        })
        .expect("login renders");
    let at = |needle: &str| {
        page.find(needle)
            .unwrap_or_else(|| panic!("{needle} is offered"))
    };
    let (apple, google, steam, discord) = (
        at(r#"href="/auth/apple"#),
        at(r#"href="/auth/google"#),
        at(r#"href="/auth/steam"#),
        at(r#"href="/auth/discord"#),
    );
    assert!(apple < google && google < steam && steam < discord);
    for provider in ["apple", "google", "steam", "discord"] {
        let button = super::provider_button(&page, provider);
        assert!(button.contains("auth-provider-mark"), "{provider} has no logo");
        assert!(button.ends_with(&format!("sign-in-with-{provider}</a>")));
    }
}

#[test]
fn signing_in_offers_apple_only_where_it_is_on() {
    let env = test_support::env();
    let render = |apple_enabled: bool, linking_provider: serde_json::Value| {
        env.get_template("login.jinja")
            .expect("login loads")
            .render(context! {
                next => "/draw",
                apple_enabled,
                linking_provider => Serde(linking_provider),
                ..chrome()
            })
            .expect("login renders")
    };
    assert!(!render(false, json!(null)).contains(r#"href="/auth/apple"#));
    let on = render(true, json!(null));
    assert!(on.contains("auth-apple"));
    assert!(on.contains("/auth/apple?next="));
    assert!(on.contains("sign-in-with-apple"));
    assert!(!render(true, json!("Apple")).contains(r#"href="/auth/apple"#));
}

#[test]
fn signing_in_offers_google_only_where_it_is_on() {
    let env = test_support::env();
    let render = |google_enabled: bool, linking_provider: serde_json::Value| {
        env.get_template("login.jinja")
            .expect("login loads")
            .render(context! {
                next => "/draw",
                google_enabled,
                linking_provider => Serde(linking_provider),
                ..chrome()
            })
            .expect("login renders")
    };
    assert!(!render(false, json!(null)).contains(r#"href="/auth/google"#));
    let on = render(true, json!(null));
    assert!(on.contains("auth-google"));
    assert!(on.contains("/auth/google?next="));
    assert!(on.contains("sign-in-with-google"));
    assert!(!render(true, json!("Google")).contains(r#"href="/auth/google"#));
    // The site's own button, with the kit's G beside the words in every
    // language.
    assert!(on.contains("/static/signin/google-mark.svg"));
    let ko = env
        .get_template("login.jinja")
        .expect("login loads")
        .render(context! { google_enabled => true, ftl_lang => "ko", ..chrome() })
        .expect("login renders");
    assert!(ko.contains("/static/signin/google-mark.svg"));
    assert!(ko.contains("ds-button auth-secondary auth-google"));
}

#[test]
fn signing_in_offers_discord_only_where_it_is_on() {
    let env = test_support::env();
    let render = |discord_enabled: bool, linking_provider: serde_json::Value| {
        env.get_template("login.jinja")
            .expect("login loads")
            .render(context! {
                next => "/collaborate/9c881320-2b43-4afa-b2bb-7128c8a3e985",
                discord_enabled,
                linking_provider => Serde(linking_provider),
                ..chrome()
            })
            .expect("login renders")
    };
    assert!(!render(false, json!(null)).contains(r#"href="/auth/discord"#));
    let on = render(true, json!(null));
    assert!(on.contains("auth-discord"));
    // Back to the room the friend was asked into, once signed in.
    assert!(on.contains("/auth/discord?next="));
    assert!(on.contains("9c881320-2b43-4afa-b2bb-7128c8a3e985"));
    assert!(on.contains("sign-in-with-discord"));
    assert!(on.contains("/static/signin/discord-mark.svg"));
    assert!(!render(true, json!("Discord")).contains(r#"href="/auth/discord"#));
}

/// A browser that signed in for an app asks before handing anything over:
/// which account, and not to go on from a link somebody sent.
#[test]
fn a_browser_signing_in_for_an_app_asks_first() {
    let env = test_support::env();
    let rendered = env
        .get_template("identity_handoff_confirm.jinja")
        .expect("confirm loads")
        .render(context! {
            provider => "Google",
            provider_name => "오이",
            provider_email => "oeee@example.test",
            ..chrome()
        })
        .expect("confirm renders");
    assert!(rendered.contains(r#"action="/auth/handoff/confirm""#));
    assert!(rendered.contains("handoff-approve-body(provider=Google)"));
    assert!(rendered.contains("오이"));
    assert!(rendered.contains("oeee@example.test"));
    assert!(rendered.contains("handoff-approve-warning"));
    assert!(rendered.contains(r#"name="cancel""#));
}

/// Back to the app, with the code its claim needs when there is one, and
/// without any when there is not.
#[test]
fn the_browser_hands_the_app_its_code() {
    let env = test_support::env();
    let render = |code: serde_json::Value| {
        env.get_template("identity_handoff_done.jinja")
            .expect("done loads")
            .render(context! { code => Serde(code), ..chrome() })
            .expect("done renders")
    };
    let code = "0123456789abcdef".repeat(4);
    let with = render(json!(code));
    assert!(with.contains(&format!(r#"href="oeee-cafe://handoff/done?code={code}""#)));
    assert!(with.contains(&format!(
        r#"handoffDone("oeee-cafe://handoff/done?code={code}")"#
    )));

    let without = render(json!(null));
    assert!(without.contains(r#"href="oeee-cafe://handoff/done""#));
    assert!(without.contains(r#"handoffDone("oeee-cafe://handoff/done")"#));
    // Not `code=` anywhere: the page's own sign-in script (app_sign_in.jinja)
    // describes the link it would carry.
    assert!(!without.contains(r#""oeee-cafe://handoff/done?code"#));
}

/// Apple's answer, posted on from this site: every field it carried, as
/// a value and never as markup.
#[test]
fn apples_answer_is_posted_on_as_it_came() {
    let env = test_support::env();
    let rendered = env
        .get_template("identity_apple_return.jinja")
        .expect("return page loads")
        .render(context! {
            answer => Serde(json!({
                "state": "the-state",
                "id_token": "a.b.c",
                "user": r#"{"name":{"firstName":"\"><script>"}}"#,
                "error": null,
            })),
            ftl_lang => "en",
        })
        .expect("return page renders");
    assert!(rendered.contains(r#"action="/auth/apple""#));
    assert!(rendered.contains(r#"name="state" value="the-state""#));
    assert!(rendered.contains(r#"name="id_token" value="a.b.c""#));
    assert!(rendered.contains(r#"name="user""#));
    assert!(!rendered.contains("<script>\""));
    assert!(!rendered.contains(r#"name="error""#));
    assert_eq!(rendered.matches("<script").count(), 1);
    // `no-referrer` would send the post with `Origin: null`, which
    // from_this_site turns away.
    assert!(!rendered.contains("no-referrer"));
}
