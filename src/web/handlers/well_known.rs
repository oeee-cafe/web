use crate::app_error::AppError;
use crate::models::sitemap::{
    sitemap_communities, sitemap_hashtags, sitemap_posts, sitemap_profiles, SitemapEntry,
};
use crate::web::state::AppState;
use axum::extract::State;
use axum::{
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use serde_json::json;

/// Caps on how much of each collection the sitemap lists. A sitemap is limited
/// to 50,000 URLs by the protocol; staying well under keeps this a single
/// document with no index file, and crawlers reach older posts by following
/// links from the newer ones.
const SITEMAP_POST_LIMIT: i64 = 20_000;
const SITEMAP_PROFILE_LIMIT: i64 = 10_000;
const SITEMAP_COMMUNITY_LIMIT: i64 = 5_000;
const SITEMAP_HASHTAG_LIMIT: i64 = 5_000;

/// Handler for Apple App Site Association (Universal Links)
/// This endpoint is used by iOS to verify the app's association with the domain
/// More info: https://developer.apple.com/documentation/xcode/supporting-associated-domains
pub async fn apple_app_site_association() -> impl IntoResponse {
    let association = json!({
        "webcredentials": {
            "apps": ["K4CQ85R27U.cafe.oeee"]
        }
    });

    (StatusCode::OK, Json(association))
}

/// Handler for Android Digital Asset Links (App Links & Credentials)
/// This endpoint is used by Android to verify the app's association with the domain
/// More info: https://developer.android.com/training/app-links/verify-android-applinks
///
/// To get the SHA256 certificate fingerprint, run:
/// For debug: keytool -list -v -keystore ~/.android/debug.keystore -alias androiddebugkey -storepass android -keypass android
/// For release: keytool -list -v -keystore /path/to/your/release.keystore
pub async fn android_assetlinks() -> impl IntoResponse {
    let assetlinks = json!([
        {
            "relation": ["delegate_permission/common.get_login_creds"],
            "target": {
                "namespace": "android_app",
                "package_name": "cafe.oeee",
                "sha256_cert_fingerprints": [
                    "35:C4:51:56:59:EB:B9:B6:08:30:0F:51:44:29:95:74:4A:2F:3C:1A:23:01:A5:C6:24:C5:0F:2E:DC:2D:72:49",
                ]
            }
        }
    ]);

    (StatusCode::OK, Json(assetlinks))
}

/// Keep crawlers out of the pages that are private, per-user, or pure machinery,
/// and point them at the sitemap.
pub async fn robots_txt(State(state): State<AppState>) -> impl IntoResponse {
    let body = format!(
        "User-agent: *\n\
         Disallow: /admin\n\
         Disallow: /account\n\
         Disallow: /api/\n\
         Disallow: /ap/\n\
         Disallow: /draw\n\
         Disallow: /home\n\
         Disallow: /login\n\
         Disallow: /logout\n\
         Disallow: /notifications\n\
         Disallow: /password-reset\n\
         Disallow: /posts/drafts\n\
         Disallow: /signup\n\
         Allow: /\n\
         \n\
         Sitemap: {}/sitemap.xml\n",
        state.config.base_url.trim_end_matches('/')
    );

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        body,
    )
}

pub async fn sitemap_xml(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    let base_url = state.config.base_url.trim_end_matches('/').to_string();
    let mut tx = state.db_pool.begin().await?;

    let posts = sitemap_posts(&mut tx, SITEMAP_POST_LIMIT).await?;
    let profiles = sitemap_profiles(&mut tx, SITEMAP_PROFILE_LIMIT).await?;
    let communities = sitemap_communities(&mut tx, SITEMAP_COMMUNITY_LIMIT).await?;
    // The directory at /hashtags was listed but not one tag page, so nothing a
    // tag collects was reachable by a crawler that had not already found the
    // drawings individually.
    let hashtags = sitemap_hashtags(&mut tx, SITEMAP_HASHTAG_LIMIT).await?;
    tx.commit().await?;

    let mut xml = String::with_capacity(
        // Roughly 150 bytes per entry, plus the static pages and the envelope.
        (posts.len() + profiles.len() + communities.len() + hashtags.len() + 8) * 150,
    );
    xml.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    xml.push('\n');
    xml.push_str(r#"<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">"#);
    xml.push('\n');

    for path in [
        "/",
        "/about",
        "/collaborate",
        "/communities",
        "/hashtags",
        "/policy",
        "/privacy",
    ] {
        xml.push_str(&format!("  <url><loc>{}{}</loc></url>\n", base_url, path));
    }

    for entry in posts
        .iter()
        .chain(&profiles)
        .chain(&communities)
        .chain(&hashtags)
    {
        push_url(&mut xml, &base_url, entry);
    }

    xml.push_str("</urlset>\n");

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        xml,
    ))
}

fn push_url(xml: &mut String, base_url: &str, entry: &SitemapEntry) {
    xml.push_str("  <url><loc>");
    xml.push_str(base_url);
    escape_xml_into(xml, &entry.path);
    xml.push_str("</loc><lastmod>");
    xml.push_str(&entry.last_modified.format("%Y-%m-%d").to_string());
    xml.push_str("</lastmod></url>\n");
}

/// Login names and community slugs end up inside `<loc>`, so the five XML
/// metacharacters have to be escaped even though they are unusual in practice.
fn escape_xml_into(out: &mut String, value: &str) {
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::escape_xml_into;

    #[test]
    fn escapes_xml_metacharacters() {
        let mut out = String::new();
        escape_xml_into(&mut out, "/@a&b<c>d\"e'f");
        assert_eq!(out, "/@a&amp;b&lt;c&gt;d&quot;e&apos;f");
    }

    #[test]
    fn leaves_ordinary_paths_alone() {
        let mut out = String::new();
        escape_xml_into(&mut out, "/@artist/0c8f-1234");
        assert_eq!(out, "/@artist/0c8f-1234");
    }
}
