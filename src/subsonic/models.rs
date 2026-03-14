use serde_json::Value;

use crate::id::encode_id;

/// Trait to rewrite all Subsonic IDs in a JSON value to be namespaced.
pub trait NamespaceIds {
    fn namespace_ids(&mut self, backend_idx: usize);
}

/// The list of JSON keys that contain Subsonic IDs and should be namespaced.
const ID_FIELDS: &[&str] = &[
    "id",
    "parent",
    "coverArt",
    "artistId",
    "albumId",
    "songId",
    "playlistId",
];

impl NamespaceIds for Value {
    fn namespace_ids(&mut self, backend_idx: usize) {
        match self {
            Value::Object(map) => {
                // Rewrite known ID fields
                for field in ID_FIELDS {
                    if let Some(val) = map.get_mut(*field) {
                        if let Some(id_str) = val.as_str() {
                            *val = Value::String(encode_id(backend_idx, id_str));
                        }
                    }
                }
                // Recurse into all values
                for val in map.values_mut() {
                    val.namespace_ids(backend_idx);
                }
            }
            Value::Array(arr) => {
                for val in arr {
                    val.namespace_ids(backend_idx);
                }
            }
            _ => {}
        }
    }
}

/// Extract and merge artist index arrays from multiple backend responses.
/// Each backend returns {"artists": {"index": [{"name": "A", "artist": [...]}, ...]}}.
/// We merge indexes by letter and combine artist lists.
pub fn merge_artist_indexes(responses: Vec<(usize, Value)>) -> Value {
    use std::collections::BTreeMap;

    let mut index_map: BTreeMap<String, Vec<Value>> = BTreeMap::new();

    for (backend_idx, mut resp) in responses {
        if let Some(artists) = resp.get_mut("artists") {
            artists.namespace_ids(backend_idx);
            if let Some(indexes) = artists.get_mut("index") {
                if let Some(arr) = indexes.as_array() {
                    for idx in arr {
                        let name = idx
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("#")
                            .to_string();
                        if let Some(artist_arr) = idx.get("artist").and_then(|a| a.as_array()) {
                            index_map
                                .entry(name)
                                .or_default()
                                .extend(artist_arr.clone());
                        }
                    }
                }
            }
        }
    }

    // Sort artists within each index by name
    let indexes: Vec<Value> = index_map
        .into_iter()
        .map(|(name, mut artists)| {
            artists.sort_by(|a, b| {
                let a_name = a.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let b_name = b.get("name").and_then(|n| n.as_str()).unwrap_or("");
                a_name.to_lowercase().cmp(&b_name.to_lowercase())
            });
            serde_json::json!({
                "name": name,
                "artist": artists,
            })
        })
        .collect();

    serde_json::json!({
        "artists": {
            "ignoredArticles": "The El La Los Las Le Les",
            "index": indexes,
        }
    })
}

/// Merge album lists from multiple backends.
pub fn merge_album_lists(
    responses: Vec<(usize, Value)>,
    list_type: &str,
    size: usize,
    offset: usize,
) -> Value {
    let mut all_albums: Vec<Value> = Vec::new();

    for (backend_idx, mut resp) in responses {
        // Try albumList2 first, then albumList
        let key = if resp.get("albumList2").is_some() {
            "albumList2"
        } else {
            "albumList"
        };

        if let Some(list) = resp.get_mut(key) {
            list.namespace_ids(backend_idx);
            if let Some(albums) = list.get("album").and_then(|a| a.as_array()) {
                all_albums.extend(albums.clone());
            }
        }
    }

    // Sort based on type
    match list_type {
        "newest" => {
            all_albums.sort_by(|a, b| {
                let a_created = a.get("created").and_then(|c| c.as_str()).unwrap_or("");
                let b_created = b.get("created").and_then(|c| c.as_str()).unwrap_or("");
                b_created.cmp(a_created) // reverse for newest first
            });
        }
        "alphabeticalByName" => {
            all_albums.sort_by(|a, b| {
                let a_name = a.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let b_name = b.get("name").and_then(|n| n.as_str()).unwrap_or("");
                a_name.to_lowercase().cmp(&b_name.to_lowercase())
            });
        }
        "alphabeticalByArtist" => {
            all_albums.sort_by(|a, b| {
                let a_artist = a.get("artist").and_then(|n| n.as_str()).unwrap_or("");
                let b_artist = b.get("artist").and_then(|n| n.as_str()).unwrap_or("");
                a_artist.to_lowercase().cmp(&b_artist.to_lowercase())
            });
        }
        "random" => {
            use rand::seq::SliceRandom;
            let mut rng = rand::rng();
            all_albums.shuffle(&mut rng);
        }
        _ => {} // For other types, keep original order
    }

    // Apply offset and size
    let paginated: Vec<Value> = all_albums
        .into_iter()
        .skip(offset)
        .take(size)
        .collect();

    serde_json::json!({
        "albumList2": {
            "album": paginated,
        }
    })
}

/// Merge search results from multiple backends.
pub fn merge_search_results(
    responses: Vec<(usize, Value)>,
    artist_count: usize,
    album_count: usize,
    song_count: usize,
) -> Value {
    let mut all_artists = Vec::new();
    let mut all_albums = Vec::new();
    let mut all_songs = Vec::new();

    for (backend_idx, mut resp) in responses {
        // Try searchResult3 first, then searchResult2
        let key = if resp.get("searchResult3").is_some() {
            "searchResult3"
        } else {
            "searchResult2"
        };

        if let Some(result) = resp.get_mut(key) {
            result.namespace_ids(backend_idx);

            if let Some(artists) = result.get("artist").and_then(|a| a.as_array()) {
                all_artists.extend(artists.clone());
            }
            if let Some(albums) = result.get("album").and_then(|a| a.as_array()) {
                all_albums.extend(albums.clone());
            }
            if let Some(songs) = result.get("song").and_then(|a| a.as_array()) {
                all_songs.extend(songs.clone());
            }
        }
    }

    all_artists.truncate(artist_count);
    all_albums.truncate(album_count);
    all_songs.truncate(song_count);

    serde_json::json!({
        "searchResult3": {
            "artist": all_artists,
            "album": all_albums,
            "song": all_songs,
        }
    })
}

/// Merge playlists from multiple backends.
pub fn merge_playlists(responses: Vec<(usize, Value)>) -> Value {
    let mut all_playlists = Vec::new();

    for (backend_idx, mut resp) in responses {
        if let Some(playlists) = resp.get_mut("playlists") {
            playlists.namespace_ids(backend_idx);
            if let Some(playlist_arr) = playlists.get("playlist").and_then(|p| p.as_array()) {
                all_playlists.extend(playlist_arr.clone());
            }
        }
    }

    serde_json::json!({
        "playlists": {
            "playlist": all_playlists,
        }
    })
}

/// Merge random songs from multiple backends.
pub fn merge_random_songs(responses: Vec<(usize, Value)>, size: usize) -> Value {
    use rand::seq::SliceRandom;

    let mut all_songs = Vec::new();

    for (backend_idx, mut resp) in responses {
        if let Some(random_songs) = resp.get_mut("randomSongs") {
            random_songs.namespace_ids(backend_idx);
            if let Some(songs) = random_songs.get("song").and_then(|s| s.as_array()) {
                all_songs.extend(songs.clone());
            }
        }
    }

    let mut rng = rand::rng();
    all_songs.shuffle(&mut rng);
    all_songs.truncate(size);

    serde_json::json!({
        "randomSongs": {
            "song": all_songs,
        }
    })
}

/// Merge starred items from multiple backends.
pub fn merge_starred(responses: Vec<(usize, Value)>) -> Value {
    let mut all_artists = Vec::new();
    let mut all_albums = Vec::new();
    let mut all_songs = Vec::new();

    for (backend_idx, mut resp) in responses {
        let key = if resp.get("starred2").is_some() {
            "starred2"
        } else {
            "starred"
        };

        if let Some(starred) = resp.get_mut(key) {
            starred.namespace_ids(backend_idx);

            if let Some(artists) = starred.get("artist").and_then(|a| a.as_array()) {
                all_artists.extend(artists.clone());
            }
            if let Some(albums) = starred.get("album").and_then(|a| a.as_array()) {
                all_albums.extend(albums.clone());
            }
            if let Some(songs) = starred.get("song").and_then(|a| a.as_array()) {
                all_songs.extend(songs.clone());
            }
        }
    }

    serde_json::json!({
        "starred2": {
            "artist": all_artists,
            "album": all_albums,
            "song": all_songs,
        }
    })
}
