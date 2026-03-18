//! Subsonic API endpoint layer.
//!
//! This module implements the full set of Subsonic (and OpenSubsonic) REST
//! endpoints that Fugue exposes to clients. Every endpoint is registered at
//! both `/rest/<name>` and `/rest/<name>.view` to match client expectations.
//!
//! # Request Handling
//!
//! Subsonic clients vary in how they send requests:
//! - Some use **GET** with query parameters, others use **POST** with a
//!   form-urlencoded body
//! - Some use the `.view` suffix on endpoint paths, others don't
//!
//! The [`params`] middleware normalizes this by merging POST body parameters
//! into the query string, so handlers always read from query params regardless
//! of the client's request method.
//!
//! # Authentication
//!
//! All endpoints (except `/admin/*`) are authenticated via the
//! [`auth::AuthenticatedUser`] extractor, which supports three methods:
//!
//! 1. **Token + salt** (preferred) — `u`, `t`, `s` params; token = MD5(password + salt)
//! 2. **Plaintext password** — `u`, `p` params; password may be hex-encoded
//!    with `enc:` prefix
//! 3. **API key** — `apiKey` param; SHA-256 hashed and matched against stored
//!    keys in the database (OpenSubsonic `apiKeyAuthentication` extension)
//!
//! Fugue authenticates clients against its own user list (`[auth.users]` in
//! config), then authenticates to each backend independently using the
//! backend's stored credentials.
//!
//! # Response Formats
//!
//! Responses are returned in either XML or JSON based on the `f` query param
//! (default: XML). The [`response`] module handles format negotiation and
//! wrapping in the standard `<subsonic-response>` envelope.
//!
//! # Admin Endpoints
//!
//! A handful of internal endpoints (`/admin/sync`, `/admin/ticket`,
//! `/admin/status`, etc.) are used by the CLI and are not authenticated.

use axum::{middleware, routing::any, routing::post, Router};

use crate::state::AppState;

pub mod annotation;
pub mod auth;
pub mod browsing;
pub mod extras;
pub mod favorites_db;
pub mod lists;
pub mod media;
pub mod params;
pub mod playlist_db;
pub mod playlists;
pub mod response;
pub mod search;
pub mod system;
pub mod models;

/// Register both `/rest/X` and `/rest/X.view` for each endpoint.
/// Uses `any()` so both GET and POST work (many Subsonic clients use POST).
macro_rules! subsonic_route {
    ($router:expr, $path:expr, $handler:expr) => {
        $router
            .route(concat!("/rest/", $path), any($handler))
            .route(concat!("/rest/", $path, ".view"), any($handler))
    };
}

pub fn router() -> Router<AppState> {
    let r = Router::new();
    // System
    let r = subsonic_route!(r, "ping", system::ping);
    let r = subsonic_route!(r, "getLicense", system::get_license);
    let r = subsonic_route!(r, "getScanStatus", system::get_scan_status);
    let r = subsonic_route!(r, "getUser", system::get_user);
    let r = subsonic_route!(r, "getOpenSubsonicExtensions", system::get_open_subsonic_extensions);
    // Browsing
    let r = subsonic_route!(r, "getMusicFolders", browsing::get_music_folders);
    let r = subsonic_route!(r, "getArtists", browsing::get_artists);
    let r = subsonic_route!(r, "getIndexes", browsing::get_indexes);
    let r = subsonic_route!(r, "getArtist", browsing::get_artist);
    let r = subsonic_route!(r, "getAlbum", browsing::get_album);
    let r = subsonic_route!(r, "getSong", browsing::get_song);
    let r = subsonic_route!(r, "getGenres", browsing::get_genres);
    // Search
    let r = subsonic_route!(r, "search2", search::search2);
    let r = subsonic_route!(r, "search3", search::search3);
    // Lists
    let r = subsonic_route!(r, "getAlbumList", lists::get_album_list);
    let r = subsonic_route!(r, "getAlbumList2", lists::get_album_list2);
    let r = subsonic_route!(r, "getRandomSongs", lists::get_random_songs);
    let r = subsonic_route!(r, "getStarred", annotation::get_starred);
    let r = subsonic_route!(r, "getStarred2", annotation::get_starred2);
    // Media
    let r = subsonic_route!(r, "stream", media::stream);
    let r = subsonic_route!(r, "download", media::download);
    let r = subsonic_route!(r, "getCoverArt", media::get_cover_art);
    // Playlists
    let r = subsonic_route!(r, "getPlaylists", playlists::get_playlists);
    let r = subsonic_route!(r, "getPlaylist", playlists::get_playlist);
    let r = subsonic_route!(r, "createPlaylist", playlists::create_playlist);
    let r = subsonic_route!(r, "updatePlaylist", playlists::update_playlist);
    let r = subsonic_route!(r, "deletePlaylist", playlists::delete_playlist);
    // Annotation
    let r = subsonic_route!(r, "star", annotation::star);
    let r = subsonic_route!(r, "unstar", annotation::unstar);
    let r = subsonic_route!(r, "setRating", annotation::set_rating);
    let r = subsonic_route!(r, "scrobble", annotation::scrobble);
    // Extras
    let r = subsonic_route!(r, "getSimilarSongs", extras::get_similar_songs);
    let r = subsonic_route!(r, "getSimilarSongs2", extras::get_similar_songs2);
    let r = subsonic_route!(r, "getTopSongs", extras::get_top_songs);
    let r = subsonic_route!(r, "getNowPlaying", extras::get_now_playing);
    let r = subsonic_route!(r, "getBookmarks", extras::get_bookmarks);
    let r = subsonic_route!(r, "createBookmark", extras::create_bookmark);
    let r = subsonic_route!(r, "deleteBookmark", extras::delete_bookmark);
    let r = subsonic_route!(r, "getPlayQueue", extras::get_play_queue);
    let r = subsonic_route!(r, "savePlayQueue", extras::save_play_queue);
    let r = subsonic_route!(r, "getInternetRadioStations", extras::get_internet_radio_stations);
    let r = subsonic_route!(r, "reportPlayback", extras::report_playback);
    let r = subsonic_route!(r, "getLyrics", extras::get_lyrics);
    let r = subsonic_route!(r, "getLyricsBySongId", extras::get_lyrics_by_song_id);
    let r = subsonic_route!(r, "getAlbumInfo", extras::get_album_info);
    let r = subsonic_route!(r, "getAlbumInfo2", extras::get_album_info2);
    let r = subsonic_route!(r, "getArtistInfo", extras::get_artist_info);
    let r = subsonic_route!(r, "getArtistInfo2", extras::get_artist_info2);
    let r = subsonic_route!(r, "getChatMessages", extras::get_chat_messages);
    let r = subsonic_route!(r, "addChatMessage", extras::add_chat_message);
    // Admin endpoints (no Subsonic auth, not exposed to clients)
    let r = r.route("/admin/sync", post(system::admin_sync));
    let r = r.route("/admin/ticket", any(system::admin_ticket));
    let r = r.route("/admin/status", any(system::admin_status));
    let r = r.route("/admin/refresh-friends", post(system::admin_refresh_friends));
    let r = r.route("/admin/playlist-sync", any(system::admin_playlist_sync));
    // Catch-all for unknown /rest/ endpoints — log and return proper Subsonic error
    let r = r.fallback(fallback_handler);
    r.layer(middleware::from_fn(params::merge_post_form_params))
}

async fn fallback_handler(req: axum::extract::Request) -> axum::response::Response {
    let path = req.uri().path().to_string();
    tracing::warn!("unhandled endpoint: {}", path);
    let body = format!(
        r#"<subsonic-response xmlns="http://subsonic.org/restapi" status="ok" version="1.16.1" type="fugue" serverVersion="0.1.0" openSubsonic="true"></subsonic-response>"#,
    );
    axum::response::Response::builder()
        .header("content-type", "text/xml; charset=utf-8")
        .body(axum::body::Body::from(body))
        .unwrap()
}
