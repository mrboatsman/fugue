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
    let r = subsonic_route!(r, "getLyrics", extras::get_lyrics);
    let r = subsonic_route!(r, "getAlbumInfo", extras::get_album_info);
    let r = subsonic_route!(r, "getAlbumInfo2", extras::get_album_info2);
    let r = subsonic_route!(r, "getArtistInfo", extras::get_artist_info);
    let r = subsonic_route!(r, "getArtistInfo2", extras::get_artist_info2);
    // Admin endpoints (no Subsonic auth, not exposed to clients)
    let r = r.route("/admin/sync", post(system::admin_sync));
    r.layer(middleware::from_fn(params::merge_post_form_params))
}
