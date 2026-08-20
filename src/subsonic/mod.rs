pub mod browsing;
pub mod directory;
pub mod lists;
pub mod lyrics;
pub mod media;
pub mod playlist;
pub mod response;
pub mod star;
pub mod system;

use axum::{routing::MethodRouter, Router};

use crate::AppState;

/// Subsonic ID 约定：ar-{id} / al-{id} / tr-{id}
pub fn track_id(id: i64) -> String {
    format!("tr-{id}")
}
pub fn album_id(id: i64) -> String {
    format!("al-{id}")
}
pub fn artist_id(id: i64) -> String {
    format!("ar-{id}")
}

fn parse_prefixed(id: &str, prefix: &str) -> Option<i64> {
    id.strip_prefix(prefix)
        .and_then(|s| s.parse::<i64>().ok())
        // 兼容部分客户端只发纯数字
        .or_else(|| id.parse::<i64>().ok())
}
pub fn parse_track_id(id: &str) -> Option<i64> {
    parse_prefixed(id, "tr-")
}
pub fn parse_album_id(id: &str) -> Option<i64> {
    parse_prefixed(id, "al-")
}
pub fn parse_artist_id(id: &str) -> Option<i64> {
    parse_prefixed(id, "ar-")
}

/// 同时注册 /path 与 /path.view（客户端两种 URL 形态都合法）。
fn r(router: Router<AppState>, path: &str, h: MethodRouter<AppState>) -> Router<AppState> {
    router.route(path, h.clone()).route(&format!("{path}.view"), h)
}

pub fn router(state: AppState) -> Router {
    use axum::routing::get;

    let mut rest = Router::new();
    rest = r(rest, "/ping", get(system::ping));
    rest = r(rest, "/getLicense", get(system::get_license));
    rest = r(rest, "/getOpenSubsonicExtensions", get(system::get_extensions));
    rest = r(rest, "/getMusicFolders", get(browsing::get_music_folders));
    rest = r(rest, "/getArtists", get(browsing::get_artists));
    rest = r(rest, "/getArtist", get(browsing::get_artist));
    rest = r(rest, "/getAlbum", get(browsing::get_album));
    rest = r(rest, "/getIndexes", get(directory::get_indexes));
    rest = r(rest, "/getMusicDirectory", get(directory::get_music_directory));
    rest = r(rest, "/getGenres", get(lists::get_genres));
    rest = r(rest, "/getAlbumList2", get(lists::get_album_list2));
    rest = r(rest, "/getRandomSongs", get(lists::get_random_songs));
    rest = r(rest, "/search3", get(lists::search3));
    rest = r(rest, "/getSongsByGenre", get(lists::get_songs_by_genre));
    // Symfonium 同步会探测的端点，先返回空结果避免 unknown endpoint
    rest = r(rest, "/getTopSongs", get(lists::get_top_songs));
    rest = r(rest, "/getSimilarSongs2", get(lists::get_similar_songs2));
    rest = r(rest, "/getArtistInfo2", get(lists::get_artist_info2));
    rest = r(rest, "/getPlaylists", get(playlist::get_playlists));
    rest = r(rest, "/getPlaylist", get(playlist::get_playlist));
    rest = r(rest, "/createPlaylist", get(playlist::create_playlist));
    rest = r(rest, "/updatePlaylist", get(playlist::update_playlist));
    rest = r(rest, "/deletePlaylist", get(playlist::delete_playlist));
    rest = r(rest, "/getBookmarks", get(lists::get_bookmarks));
    rest = r(rest, "/createBookmark", get(lists::create_bookmark));
    rest = r(rest, "/deleteBookmark", get(lists::delete_bookmark));
    rest = r(rest, "/getPlayQueue", get(lists::get_play_queue));
    rest = r(rest, "/savePlayQueue", get(lists::save_play_queue));
    rest = r(rest, "/scrobble", get(lists::scrobble));
    rest = r(rest, "/getLyrics", get(lyrics::get_lyrics));
    rest = r(rest, "/getLyricsBySongId", get(lyrics::get_lyrics_by_song_id));
    // 探测类端点空结果桩
    rest = r(rest, "/getPodcasts", get(lists::get_podcasts));
    rest = r(rest, "/getNewestPodcasts", get(lists::get_newest_podcasts));
    rest = r(rest, "/getInternetRadioStations", get(lists::get_internet_radio_stations));
    rest = r(rest, "/getShares", get(lists::get_shares));
    rest = r(rest, "/getVideos", get(lists::get_videos));
    rest = r(rest, "/getChatMessages", get(lists::get_chat_messages));
    rest = r(rest, "/getAlbumInfo2", get(lists::get_album_info2));
    rest = r(rest, "/star", get(star::star));
    rest = r(rest, "/unstar", get(star::unstar));
    rest = r(rest, "/getStarred", get(star::get_starred_handler));
    rest = r(rest, "/getStarred2", get(star::get_starred2_handler));
    rest = r(rest, "/stream", get(media::stream));
    rest = r(rest, "/getCoverArt", get(media::get_cover_art));

    Router::new()
        .nest("/rest", rest)
        .fallback(system::not_found)
        .layer(
            tower_http::trace::TraceLayer::new_for_http()
                .make_span_with(|req: &axum::extract::Request| {
                    // 凭证参数脱敏后才进日志
                    let sanitized = req.uri().query().map(|q| {
                        form_urlencoded::parse(q.as_bytes())
                            .map(|(k, v)| {
                                if matches!(k.as_ref(), "u" | "t" | "s" | "p" | "apiKey") {
                                    format!("{k}=***")
                                } else {
                                    format!("{k}={v}")
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("&")
                    });
                    tracing::info_span!("http", method = %req.method(), path = req.uri().path(), query = sanitized)
                })
                .on_request(())
                .on_response(
                    tower_http::trace::DefaultOnResponse::new().level(tracing::Level::INFO),
                ),
        )
        .with_state(state)
}
