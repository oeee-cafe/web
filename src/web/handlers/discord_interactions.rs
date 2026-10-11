//! `/oeee-cafe`, typed in any Discord chat: a collaborative room, and a Join
//! button for everyone in the chat.
//!
//! Oeee Cafe is a Discord app people add to their own account (a user
//! install), so the command goes wherever they go -- servers, group DMs,
//! DMs -- without a bot in any of them. Discord sends each use here, to
//! `/api/discord/interactions`, signed with the application's key, and the
//! answer is the message posted in the chat.
//!
//! Who typed it is Discord's word, in a request Discord signed, and the room
//! is opened by the Oeee Cafe account that Discord account signs into
//! (`user_identities`). Somebody whose Discord is not linked to an account
//! is told so, and only they see it, with the way to sign in with Discord;
//! nothing is made for them. The room is opened as the lobby opens one
//! (`insert_session`), in no community and not listed in the lobby: the
//! link is for the chat. Anyone who follows it signs in on the site as
//! usual, and in the Windows app the room then asks friends in from Discord
//! as any other does.
//!
//! The commands are Discord's to show, and are told to Discord on boot
//! ([`register`]), with the application's client credentials.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use ed25519_dalek::{Signature, VerifyingKey};
use fluent::bundle::FluentBundle;
use fluent::{FluentArgs, FluentResource};
use intl_memoizer::concurrent::IntlLangMemoizer;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::config::DiscordConfig;
use crate::locale::LOCALES;
use crate::models::identity::{find_user_by_identity, Provider};
use crate::web::handlers::collaborate::{
    insert_session, CreateSessionRequest, CANVAS_SIZES, DEFAULT_CANVAS_SIZE,
};
use crate::web::state::AppState;

/// The command's name, the same in every language: the app's own, so it
/// stands apart from every other app's commands in the picker.
pub const COMMAND: &str = "oeee-cafe";

/// What a session may be, as the lobby offers it (`CANVAS_SIZES`,
/// `MAX_PARTICIPANTS_CHOICES`), and what `/oeee-cafe` alone opens.
const SEATS: [i32; 4] = [2, 4, 6, 8];
const DEFAULT_SEATS: i32 = 4;

/// A canvas size as the command's choice carries it, "1024x768".
fn size_value((width, height): (u32, u32)) -> String {
    format!("{width}x{height}")
}

/// The longest title the command takes, which Discord enforces before it
/// sends anything.
const TITLE_MOST: u16 = 100;

/// An interaction's types and its answers' (Discord's Interactions API).
const PING: u8 = 1;
const APPLICATION_COMMAND: u8 = 2;
const PONG: u8 = 1;
const CHANNEL_MESSAGE: u8 = 4;
/// The message flag that shows it only to whoever typed the command.
const EPHEMERAL: u32 = 1 << 6;
/// A button that is a link, and the row that holds it.
const ACTION_ROW: u8 = 1;
const BUTTON: u8 = 2;
const LINK: u8 = 5;

/// Whether Discord signed this: `timestamp` then the body, with the
/// application's key. Anything else, and anything at all when the key is
/// not one, is not Discord's.
pub fn signed(public_key: &str, signature: &str, timestamp: &str, body: &[u8]) -> bool {
    let Some(key) = hex::decode(public_key)
        .ok()
        .and_then(|key| <[u8; 32]>::try_from(key).ok())
        .and_then(|key| VerifyingKey::from_bytes(&key).ok())
    else {
        return false;
    };
    let Some(signature) = hex::decode(signature)
        .ok()
        .and_then(|signature| <[u8; 64]>::try_from(signature).ok())
        .map(|signature| Signature::from_bytes(&signature))
    else {
        return false;
    };
    let mut message = Vec::with_capacity(timestamp.len() + body.len());
    message.extend_from_slice(timestamp.as_bytes());
    message.extend_from_slice(body);
    key.verify_strict(&message, &signature).is_ok()
}

#[derive(Deserialize)]
struct Interaction {
    #[serde(rename = "type")]
    kind: u8,
    #[serde(default)]
    data: Option<CommandData>,
    /// Who typed it in a server.
    #[serde(default)]
    member: Option<Member>,
    /// Who typed it anywhere else.
    #[serde(default)]
    user: Option<DiscordUser>,
    /// The language their Discord is in.
    #[serde(default)]
    locale: Option<String>,
}

#[derive(Deserialize)]
struct CommandData {
    name: String,
    #[serde(default)]
    options: Vec<CommandOption>,
}

#[derive(Deserialize)]
struct CommandOption {
    name: String,
    value: Value,
}

#[derive(Deserialize)]
struct Member {
    user: DiscordUser,
}

#[derive(Deserialize)]
struct DiscordUser {
    id: String,
}

impl Interaction {
    fn user_id(&self) -> Option<&str> {
        self.member
            .as_ref()
            .map(|member| &member.user)
            .or(self.user.as_ref())
            .map(|user| user.id.as_str())
            .filter(|id| !id.is_empty())
    }

    fn option(&self, name: &str) -> Option<&Value> {
        self.data
            .as_ref()?
            .options
            .iter()
            .find(|option| option.name == name)
            .map(|option| &option.value)
    }
}

/// What `/oeee-cafe` was asked for, checked against what the lobby offers.
#[derive(Debug, PartialEq, Eq)]
struct Asked {
    title: Option<String>,
    width: i32,
    height: i32,
    seats: i32,
}

fn asked(interaction: &Interaction) -> Option<Asked> {
    let (width, height) = match interaction.option("size") {
        Some(value) => {
            let size = value.as_str()?;
            CANVAS_SIZES
                .into_iter()
                .find(|&offered| size_value(offered) == size)?
        }
        None => DEFAULT_CANVAS_SIZE,
    };
    let seats = match interaction.option("seats") {
        Some(value) => value
            .as_i64()
            .and_then(|seats| i32::try_from(seats).ok())
            .filter(|seats| SEATS.contains(seats))?,
        None => DEFAULT_SEATS,
    };
    let title = interaction
        .option("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(|title| title.chars().take(TITLE_MOST.into()).collect());
    Some(Asked {
        title,
        width: width as i32,
        height: height as i32,
        seats,
    })
}

/// POST /api/discord/interactions. 404 when the site has no application
/// key to check with; 401 for what Discord did not sign, which Discord
/// itself sends now and then to see that it is checked.
pub async fn interactions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(public_key) = state
        .config
        .discord
        .as_ref()
        .and_then(|discord| discord.public_key.as_deref())
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
    };
    if !signed(
        public_key,
        header("x-signature-ed25519"),
        header("x-signature-timestamp"),
        &body,
    ) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(interaction) = serde_json::from_slice::<Interaction>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match interaction.kind {
        PING => Json(json!({ "type": PONG })).into_response(),
        APPLICATION_COMMAND
            if interaction
                .data
                .as_ref()
                .is_some_and(|data| data.name == COMMAND) =>
        {
            Json(draw(&state, &interaction).await).into_response()
        }
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

/// `/oeee-cafe`: the room, or why not, in the language of whoever typed it.
async fn draw(state: &AppState, interaction: &Interaction) -> Value {
    let words = Words::new(interaction.locale.as_deref());
    let base_url = state.config.base_url.trim_end_matches('/');
    let (Some(discord_id), Some(asked)) = (interaction.user_id(), asked(interaction)) else {
        return only_them(words.get("discord-draw-failed", None), None);
    };

    let account = async {
        let mut tx = state.db_pool.begin().await?;
        let user = find_user_by_identity(&mut tx, Provider::Discord, discord_id).await?;
        tx.commit().await?;
        anyhow::Ok(user)
    }
    .await;
    let user = match account {
        Ok(Some(user)) => user,
        Ok(None) => {
            return only_them(
                words.get("discord-draw-link-first", None),
                Some(link(
                    &words.get("discord-draw-sign-in", None),
                    &format!("{base_url}/auth/discord"),
                )),
            );
        }
        Err(error) => {
            tracing::error!("/oeee-cafe could not look up a Discord account: {error:#}");
            return only_them(words.get("discord-draw-failed", None), None);
        }
    };

    let request = CreateSessionRequest {
        title: asked.title.clone(),
        width: asked.width,
        height: asked.height,
        is_public: false,
        max_participants: asked.seats,
        community_id: None,
    };
    let session_id = match insert_session(state, user.id, request).await {
        Ok(id) => id,
        Err(error) => {
            tracing::error!("/oeee-cafe could not open a room: {error:?}");
            return only_them(words.get("discord-draw-failed", None), None);
        }
    };

    let mut args = FluentArgs::new();
    args.set("name", escape_markdown(&user.display_name));
    args.set("seats", asked.seats);
    let content = match &asked.title {
        Some(title) => {
            args.set("title", escape_markdown(title));
            words.get("discord-draw-started-titled", Some(&args))
        }
        None => words.get("discord-draw-started", Some(&args)),
    };
    json!({
        "type": CHANNEL_MESSAGE,
        "data": {
            "content": content,
            // A display name is the account's to choose, and "@everyone" is
            // a display name like any other.
            "allowed_mentions": { "parse": [] },
            "components": [link(
                &words.get("discord-draw-join", None),
                &format!("{base_url}/collaborate/{session_id}"),
            )],
        }
    })
}

/// An answer only whoever typed the command sees.
fn only_them(content: String, row: Option<Value>) -> Value {
    json!({
        "type": CHANNEL_MESSAGE,
        "data": {
            "content": content,
            "flags": EPHEMERAL,
            "allowed_mentions": { "parse": [] },
            "components": row.into_iter().collect::<Vec<_>>(),
        }
    })
}

/// A row with one button that opens `url`.
fn link(label: &str, url: &str) -> Value {
    json!({
        "type": ACTION_ROW,
        "components": [{ "type": BUTTON, "style": LINK, "label": label, "url": url }],
    })
}

/// `text` with whatever Discord's markdown would read as markup taken as
/// itself, so a name like `*_*` is shown as typed.
fn escape_markdown(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(
            c,
            '\\' | '*' | '_' | '~' | '`' | '|' | '>' | '#' | '-' | '[' | ']' | '(' | ')'
        ) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// The site's language for a Discord locale ("ko", "ja", "zh-CN", "en-US",
/// ...), English for any it has no words in.
fn site_language(locale: Option<&str>) -> &'static str {
    match locale.and_then(|locale| locale.split('-').next()) {
        Some("ko") => "ko",
        Some("ja") => "ja",
        Some("zh") => "zh",
        _ => "en",
    }
}

/// The site's words in one language.
struct Words(FluentBundle<&'static FluentResource, IntlLangMemoizer>);

impl Words {
    fn new(locale: Option<&str>) -> Self {
        Self::in_language(site_language(locale))
    }

    fn in_language(language: &str) -> Self {
        let mut bundle = FluentBundle::new_concurrent(vec![language
            .parse()
            .expect("the site's languages are language tags")]);
        // Discord shows the text as it is: no bidi isolation marks around
        // a name.
        bundle.set_use_isolating(false);
        bundle
            .add_resource(&LOCALES[language])
            .expect("a locale file adds once");
        Words(bundle)
    }

    fn get(&self, id: &str, args: Option<&FluentArgs>) -> String {
        let Some(pattern) = self.0.get_message(id).and_then(|message| message.value()) else {
            return id.to_string();
        };
        let mut errors = vec![];
        self.0
            .format_pattern(pattern, args, &mut errors)
            .to_string()
    }
}

/// Discord's locales each of the site's languages is offered in, for the
/// command's own description and options.
const LOCALIZED: [(&str, &[&str]); 3] = [
    ("ko", &["ko"]),
    ("ja", &["ja"]),
    ("zh", &["zh-CN", "zh-TW"]),
];

/// `id` in every language but English, keyed as Discord keys localisations.
fn localized(id: &str) -> Value {
    let mut all = serde_json::Map::new();
    for (language, locales) in LOCALIZED {
        let text = Words::in_language(language).get(id, None);
        for locale in locales {
            all.insert(locale.to_string(), Value::String(text.clone()));
        }
    }
    Value::Object(all)
}

fn described(id: &str) -> (String, Value) {
    (Words::in_language("en").get(id, None), localized(id))
}

/// The application's commands, as Discord is told them.
pub fn commands() -> Value {
    let (description, description_localizations) = described("discord-command-draw");
    let (title, title_localizations) = described("discord-command-draw-title");
    let (size, size_localizations) = described("discord-command-draw-size");
    let (seats, seats_localizations) = described("discord-command-draw-seats");
    json!([{
        "name": COMMAND,
        // A slash command.
        "type": 1,
        "description": description,
        "description_localizations": description_localizations,
        // Added to a server, or to someone's own account, and usable in a
        // server, in a DM with the app, and in any other DM or group DM.
        "integration_types": [0, 1],
        "contexts": [0, 1, 2],
        "options": [
            {
                // A string.
                "type": 3,
                "name": "title",
                "description": title,
                "description_localizations": title_localizations,
                "max_length": TITLE_MOST,
            },
            {
                "type": 3,
                "name": "size",
                "description": size,
                "description_localizations": size_localizations,
                "choices": CANVAS_SIZES.into_iter().map(|(width, height)| json!({
                    "name": format!("{width}×{height}"),
                    "value": size_value((width, height)),
                })).collect::<Vec<_>>(),
            },
            {
                // An integer.
                "type": 4,
                "name": "seats",
                "description": seats,
                "description_localizations": seats_localizations,
                "choices": SEATS.iter().map(|seats| json!({ "name": seats.to_string(), "value": seats })).collect::<Vec<_>>(),
            },
        ],
    }])
}

/// Discord's type for an Activity's Entry Point command, which Discord makes
/// itself when Activities are turned on for the application.
const PRIMARY_ENTRY_POINT: u64 = 4;

/// Tells Discord the application's commands, replacing whatever it had
/// besides an Entry Point, which Discord will not have replaced and which
/// is not the site's: it is asked for first and handed back as it was. The
/// same list every time, so a boot that tells it again changes nothing. A
/// token for the application itself first, by its client credentials,
/// which may do no more than this.
pub async fn register(config: &DiscordConfig) -> anyhow::Result<()> {
    #[derive(Deserialize)]
    struct Token {
        access_token: String,
    }
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let token = http
        .post(&config.token_url)
        .basic_auth(&config.client_id, Some(&config.client_secret))
        .form(&[
            ("grant_type", "client_credentials"),
            ("scope", "applications.commands.update"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<Token>()
        .await?;
    let url = format!(
        "{}/applications/{}/commands",
        config.api_url.trim_end_matches('/'),
        config.client_id
    );
    let had = http
        .get(&url)
        .bearer_auth(&token.access_token)
        .send()
        .await?
        .error_for_status()?
        .json::<Vec<Value>>()
        .await?;
    let mut list = match commands() {
        Value::Array(list) => list,
        _ => unreachable!("the commands are a list"),
    };
    list.extend(
        had.into_iter()
            .filter(|command| command["type"].as_u64() == Some(PRIMARY_ENTRY_POINT)),
    );
    let response = http
        .put(&url)
        .bearer_auth(&token.access_token)
        .json(&list)
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Discord would not take the commands ({status}): {body}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7; 32])
    }

    fn sign(timestamp: &str, body: &[u8]) -> String {
        let mut message = timestamp.as_bytes().to_vec();
        message.extend_from_slice(body);
        hex::encode(key().sign(&message).to_bytes())
    }

    #[test]
    fn only_what_discord_signed_is_taken() {
        let public = hex::encode(key().verifying_key().to_bytes());
        let body = br#"{"type":1}"#;
        let signature = sign("1760140800", body);
        assert!(signed(&public, &signature, "1760140800", body));
        // Another time, another body, another key, or none at all.
        assert!(!signed(&public, &signature, "1760140801", body));
        assert!(!signed(&public, &signature, "1760140800", br#"{"type":2}"#));
        let other = hex::encode(SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes());
        assert!(!signed(&other, &signature, "1760140800", body));
        assert!(!signed(&public, "", "1760140800", body));
        assert!(!signed(&public, "zz", "1760140800", body));
        assert!(!signed("", &signature, "1760140800", body));
    }

    fn interaction(value: Value) -> Interaction {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn whoever_typed_it_is_the_member_in_a_server_and_the_user_elsewhere() {
        let in_server = interaction(json!({
            "type": 2, "data": {"name": "oeee-cafe"},
            "member": {"user": {"id": "80351110224678912"}},
        }));
        assert_eq!(in_server.user_id(), Some("80351110224678912"));
        let in_dm = interaction(json!({
            "type": 2, "data": {"name": "oeee-cafe"}, "user": {"id": "42"},
        }));
        assert_eq!(in_dm.user_id(), Some("42"));
        let nobody = interaction(json!({ "type": 2, "data": {"name": "oeee-cafe"} }));
        assert_eq!(nobody.user_id(), None);
    }

    #[test]
    fn the_command_asks_only_for_what_the_lobby_offers() {
        let with = |options: Value| {
            asked(&interaction(json!({
                "type": 2, "data": {"name": "oeee-cafe", "options": options},
            })))
        };
        assert_eq!(
            with(json!([])),
            Some(Asked {
                title: None,
                width: 1024,
                height: 768,
                seats: 4
            })
        );
        assert_eq!(
            with(json!([
                {"name": "title", "value": "  밤의 고양이 "},
                {"name": "size", "value": "1000x1000"},
                {"name": "seats", "value": 8},
            ])),
            Some(Asked {
                title: Some("밤의 고양이".into()),
                width: 1000,
                height: 1000,
                seats: 8
            })
        );
        assert_eq!(
            with(json!([{"name": "title", "value": "   "}]))
                .unwrap()
                .title,
            None
        );
        assert_eq!(with(json!([{"name": "size", "value": "4096x4096"}])), None);
        assert_eq!(with(json!([{"name": "size", "value": "1024×768"}])), None);
        assert_eq!(with(json!([{"name": "seats", "value": 3}])), None);
        assert_eq!(with(json!([{"name": "seats", "value": "4"}])), None);
    }

    #[test]
    fn a_name_is_shown_as_typed_and_mentions_nobody() {
        assert_eq!(escape_markdown("*_넬리_*"), r"\*\_넬리\_\*");
        assert_eq!(escape_markdown("[a](b)"), r"\[a\]\(b\)");
    }

    #[test]
    fn discord_locales_are_the_sites_languages() {
        assert_eq!(site_language(Some("ko")), "ko");
        assert_eq!(site_language(Some("zh-TW")), "zh");
        assert_eq!(site_language(Some("en-GB")), "en");
        assert_eq!(site_language(Some("fr")), "en");
        assert_eq!(site_language(None), "en");
    }

    #[test]
    fn every_word_is_there_in_every_language() {
        for language in ["en", "ko", "ja", "zh"] {
            let words = Words::in_language(language);
            for id in [
                "discord-draw-failed",
                "discord-draw-link-first",
                "discord-draw-sign-in",
                "discord-draw-join",
                "discord-command-draw",
                "discord-command-draw-title",
                "discord-command-draw-size",
                "discord-command-draw-seats",
            ] {
                assert_ne!(words.get(id, None), id, "{language} lacks {id}");
            }
            let mut args = FluentArgs::new();
            args.set("name", "넬리");
            args.set("seats", 4);
            args.set("title", "고양이");
            for id in ["discord-draw-started", "discord-draw-started-titled"] {
                let said = words.get(id, Some(&args));
                assert!(said.contains("넬리"), "{language} {id}: {said}");
            }
            assert!(words
                .get("discord-draw-started-titled", Some(&args))
                .contains("고양이"));
        }
    }

    #[test]
    fn a_description_is_within_what_discord_takes() {
        let commands = commands();
        let draw = &commands[0];
        assert_eq!(draw["name"], COMMAND);
        let mut descriptions = vec![&draw["description"]];
        descriptions.extend(
            draw["description_localizations"]
                .as_object()
                .unwrap()
                .values(),
        );
        for option in draw["options"].as_array().unwrap() {
            descriptions.push(&option["description"]);
            descriptions.extend(
                option["description_localizations"]
                    .as_object()
                    .unwrap()
                    .values(),
            );
        }
        for description in descriptions {
            let length = description.as_str().unwrap().chars().count();
            assert!((1..=100).contains(&length), "{description}");
        }
        assert_eq!(
            draw["description_localizations"]["zh-TW"],
            draw["description_localizations"]["zh-CN"]
        );
    }

    static ENTRY_POINT: std::sync::LazyLock<Value> =
        std::sync::LazyLock::new(|| json!({"id": "1", "type": 4, "name": "launch", "handler": 2}));

    /// Discord, as far as telling it the commands goes: a token for the
    /// application's own credentials, then the list, which it keeps.
    #[tokio::test]
    async fn the_commands_are_told_with_a_token_for_the_application_itself() {
        use axum::extract::Path;
        use axum::routing::{get, post};
        use std::sync::{Arc, Mutex};

        let kept: Arc<Mutex<Option<(String, Value)>>> = Arc::default();
        let keeping = kept.clone();
        let app = axum::Router::new()
            .route(
                "/token",
                post(|headers: HeaderMap, body: String| async move {
                    // "app:secret", as basic auth spells it.
                    let basic = headers.get("authorization").and_then(|v| v.to_str().ok());
                    let form: Vec<(String, String)> = url::form_urlencoded::parse(body.as_bytes())
                        .into_owned()
                        .collect();
                    let asked = form.contains(&("grant_type".into(), "client_credentials".into()))
                        && form.contains(&("scope".into(), "applications.commands.update".into()));
                    if basic != Some("Basic YXBwOnNlY3JldA==") || !asked {
                        return (StatusCode::UNAUTHORIZED, Json(json!({})));
                    }
                    (StatusCode::OK, Json(json!({"access_token": "app-token"})))
                }),
            )
            .route(
                "/applications/{id}/commands",
                get(|headers: HeaderMap| async move {
                    let bearer = headers.get("authorization").and_then(|v| v.to_str().ok());
                    if bearer != Some("Bearer app-token") {
                        return (StatusCode::UNAUTHORIZED, Json(json!([])));
                    }
                    (
                        StatusCode::OK,
                        Json(json!([ENTRY_POINT.clone(), {"id": "2", "type": 1, "name": "draw"}])),
                    )
                })
                .put(
                    move |Path(id): Path<String>, headers: HeaderMap, Json(body): Json<Value>| {
                        let keeping = keeping.clone();
                        async move {
                            let bearer = headers.get("authorization").and_then(|v| v.to_str().ok());
                            if bearer != Some("Bearer app-token") {
                                return StatusCode::UNAUTHORIZED;
                            }
                            *keeping.lock().unwrap() = Some((id, body));
                            StatusCode::OK
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut config = DiscordConfig {
            client_id: "app".into(),
            client_secret: "secret".into(),
            token_url: format!("http://{addr}/token"),
            user_url: String::new(),
            revoke_url: String::new(),
            public_key: Some(String::new()),
            api_url: format!("http://{addr}/"),
        };
        register(&config).await.unwrap();
        let (id, body) = kept.lock().unwrap().take().expect("the commands");
        assert_eq!(id, "app");
        // Ours, and Discord's Entry Point as it was; an old command of ours
        // is gone.
        let mut expected = commands().as_array().unwrap().clone();
        expected.push(ENTRY_POINT.clone());
        assert_eq!(body, Value::Array(expected));

        config.client_secret = "wrong".into();
        assert!(register(&config).await.is_err());
    }

    #[test]
    fn a_room_message_names_nobody_and_hides_nothing() {
        let only = only_them(
            "x".into(),
            Some(link("Sign in", "https://oeee.cafe/auth/discord")),
        );
        assert_eq!(only["data"]["flags"], EPHEMERAL);
        assert_eq!(
            only["data"]["components"][0]["components"][0]["style"],
            LINK
        );
        assert_eq!(only["data"]["allowed_mentions"]["parse"], json!([]));
    }
}
