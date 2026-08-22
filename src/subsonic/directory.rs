use std::collections::{BTreeMap, HashMap};

use axum::{
    extract::{Query, State},
    response::{IntoResponse, Response},
};
use serde_json::{json, Value};
use sqlx::Row;

use crate::auth::SubsonicAuth;
use crate::subsonic::browsing::{song_json, SONG_SQL};
use crate::subsonic::response::{error, ok};
use crate::AppState;

/// 目录 ID 约定：dir-{folder}-{相对路径}（客户端原样回传）。
fn dir_id(folder: i64, path: &str) -> String {
    format!("dir-{folder}-{path}")
}

fn parse_dir_id(id: &str) -> Option<(i64, String)> {
    let rest = id.strip_prefix("dir-")?;
    let (folder, path) = rest.split_once('-')?;
    Some((folder.parse().ok()?, path.to_string()))
}

pub(crate) fn parse_dir_id_pub(id: &str) -> Option<(i64, String)> {
    parse_dir_id(id)
}

fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

pub(crate) fn like_escape_pub(s: &str) -> String {
    like_escape(s)
}

fn first_letter(name: &str) -> String {
    name.chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .filter(|s| s.chars().next().is_some_and(|c| c.is_ascii_alphabetic()))
        .unwrap_or_else(|| "#".into())
}

fn dir_child(folder: i64, parent: &str, full_path: &str, name: &str) -> Value {
    json!({
        "id": dir_id(folder, full_path),
        "parent": dir_id(folder, parent),
        "isDir": true,
        "title": name,
    })
}

/// 查询某前缀下的直接子目录名（去重、排序）。
async fn child_dirs(st: &AppState, folder: i64, prefix: &str) -> Result<Vec<String>, sqlx::Error> {
    let pattern = if prefix.is_empty() {
        "%".to_string()
    } else {
        format!("{}/%", like_escape(prefix))
    };
    let rows = sqlx::query("SELECT path FROM tracks WHERE folder = ? AND path LIKE ? ESCAPE '\\'")
        .bind(folder)
        .bind(pattern)
        .fetch_all(&st.pool)
        .await?;

    let depth = prefix.matches('/').count() + if prefix.is_empty() { 0 } else { 1 };
    let mut dirs = Vec::new();
    for r in rows {
        let path: String = r.get("path");
        let segs: Vec<&str> = path.split('/').collect();
        if segs.len() > depth + 1 {
            dirs.push(segs[depth].to_string());
        }
    }
    dirs.sort();
    dirs.dedup();
    Ok(dirs)
}

/// 查询某前缀下的直接音频文件。
async fn child_songs(st: &AppState, folder: i64, prefix: &str) -> Result<Vec<Value>, sqlx::Error> {
    let rows = if prefix.is_empty() {
        sqlx::query(&format!(
            "{SONG_SQL} WHERE t.folder = ? AND t.path NOT LIKE '%/%' ORDER BY t.track_no, t.title"
        ))
        .bind(folder)
        .fetch_all(&st.pool)
        .await?
    } else {
        let pattern = format!("{}/%", like_escape(prefix));
        let cut = prefix.chars().count() + 2; // SQLite substr 从 1 开始
        sqlx::query(&format!(
            "{SONG_SQL} WHERE t.folder = ? AND t.path LIKE ? ESCAPE '\\' AND substr(t.path, {cut}) NOT LIKE '%/%'
             ORDER BY t.track_no, t.title"
        ))
        .bind(folder)
        .bind(pattern)
        .fetch_all(&st.pool)
        .await?
    };
    Ok(rows.iter().map(song_json).collect())
}

/// 虚拟目录 ID（扁平库按艺术家/专辑虚拟分组）：
///   dirv-ar-{artist_id}  艺术家虚拟目录
///   dirv-al-{album_id}   专辑虚拟目录
fn dirv_artist_id(aid: i64) -> String {
    format!("dirv-ar-{aid}")
}
fn dirv_album_id(al_id: i64) -> String {
    format!("dirv-al-{al_id}")
}
fn parse_dirv_id(id: &str) -> Option<(String, i64)> {
    // 返回 (kind, id)，kind ∈ ar/al
    let rest = id.strip_prefix("dirv-")?;
    let (kind, n) = rest.split_once('-')?;
    Some((kind.to_string(), n.parse().ok()?))
}

/// 歌手虚拟目录 -> 该歌手下的专辑虚拟目录（含无专辑归属的散歌）。
async fn virtual_artist(st: &AppState, aid: i64) -> Result<Vec<Value>, sqlx::Error> {
    let albums = sqlx::query(
        "SELECT DISTINCT al.id, al.name FROM albums al
         JOIN tracks t ON t.album_id = al.id WHERE t.artist_id = ? ORDER BY al.name COLLATE NOCASE",
    )
    .bind(aid)
    .fetch_all(&st.pool)
    .await?;
    let mut children: Vec<Value> = albums
        .iter()
        .map(|r| {
            json!({
                "id": dirv_album_id(r.get::<i64, _>("id")),
                "parent": dirv_artist_id(aid),
                "isDir": true,
                "title": r.get::<String, _>("name"),
            })
        })
        .collect();
    // 该歌手无专辑归属的散歌
    let strays = sqlx::query(&format!(
        "{SONG_SQL} WHERE t.artist_id = ? AND t.album_id IS NULL ORDER BY t.title"
    ))
    .bind(aid)
    .fetch_all(&st.pool)
    .await?;
    children.extend(strays.iter().map(song_json));
    Ok(children)
}

/// 专辑虚拟目录 -> 该专辑的歌曲。
async fn virtual_album(st: &AppState, al_id: i64) -> Result<Vec<Value>, sqlx::Error> {
    let rows = sqlx::query(&format!(
        "{SONG_SQL} WHERE t.album_id = ? ORDER BY t.track_no, t.title"
    ))
    .bind(al_id)
    .fetch_all(&st.pool)
    .await?;
    Ok(rows.iter().map(song_json).collect())
}

pub async fn get_indexes(
    State(st): State<AppState>,
    _auth: SubsonicAuth,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let folder: i64 = q
        .get("musicFolderId")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let dirs = match child_dirs(&st, folder, "").await {
        Ok(d) => d,
        Err(e) => return error(0, &e.to_string()),
    };
    let root_songs = match child_songs(&st, folder, "").await {
        Ok(s) => s,
        Err(e) => return error(0, &e.to_string()),
    };

    let last_modified: Option<i64> = sqlx::query("SELECT MAX(mtime) FROM tracks WHERE folder = ?")
        .bind(folder)
        .fetch_one(&st.pool)
        .await
        .ok()
        .and_then(|r| r.get::<Option<i64>, _>(0));
    let lm = last_modified.unwrap_or(0) * 1000;

    // 扁平库（无物理子目录）：按艺术家/专辑生成虚拟目录树，让文件视图可浏览
    if dirs.is_empty() {
        let artists = sqlx::query(
            "SELECT ar.id, ar.name, COUNT(DISTINCT t.album_id) AS album_count
             FROM artists ar JOIN tracks t ON t.artist_id = ar.id
             WHERE t.folder = ? GROUP BY ar.id ORDER BY ar.name COLLATE NOCASE",
        )
        .bind(folder)
        .fetch_all(&st.pool)
        .await
        .unwrap_or_default();
        let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for r in &artists {
            let aid: i64 = r.get("id");
            let name: String = r.get("name");
            groups.entry(first_letter(&name)).or_default().push(json!({
                "id": dirv_artist_id(aid),
                "name": name,
                "albumCount": r.get::<i64, _>("album_count"),
            }));
        }
        let index: Vec<Value> = groups
            .into_iter()
            .map(|(name, artist)| json!({ "name": name, "artist": artist }))
            .collect();
        return ok(json!({
            "indexes": {"lastModified": lm, "ignoredArticles": "The El La Los Las Le Les", "index": index, "child": root_songs}
        }))
        .into_response();
    }

    let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for d in &dirs {
        groups
            .entry(first_letter(d))
            .or_default()
            .push(json!({ "id": dir_id(folder, d), "name": d }));
    }
    let index: Vec<Value> = groups
        .into_iter()
        .map(|(name, artist)| json!({ "name": name, "artist": artist }))
        .collect();

    let last_modified: Option<i64> = sqlx::query("SELECT MAX(mtime) FROM tracks WHERE folder = ?")
        .bind(folder)
        .fetch_one(&st.pool)
        .await
        .ok()
        .and_then(|r| r.get::<Option<i64>, _>(0));

    ok(json!({
        "indexes": {
            "lastModified": last_modified.unwrap_or(0) * 1000,
            "ignoredArticles": "The El La Los Las Le Les",
            "index": index,
            "child": root_songs,
        }
    }))
    .into_response()
}

pub async fn get_music_directory(
    State(st): State<AppState>,
    _auth: SubsonicAuth,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(id) = q.get("id") else {
        return error(10, "missing id");
    };

    // 虚拟目录：dirv-ar-{aid} / dirv-al-{aid}
    if let Some((kind, nid)) = parse_dirv_id(id) {
        let (children, name, parent) = match kind.as_str() {
            "ar" => {
                let name = sqlx::query("SELECT name FROM artists WHERE id = ?")
                    .bind(nid)
                    .fetch_optional(&st.pool)
                    .await
                    .ok()
                    .flatten()
                    .map(|r| r.get::<String, _>("name"))
                    .unwrap_or_default();
                let children = match virtual_artist(&st, nid).await {
                    Ok(c) => c,
                    Err(e) => return error(0, &e.to_string()),
                };
                (children, name, Some(dirv_artist_id(nid)))
            }
            "al" => {
                let name = sqlx::query(
                    "SELECT al.name, ar.name AS artist_name FROM albums al
                     LEFT JOIN artists ar ON ar.id = al.artist_id WHERE al.id = ?",
                )
                .bind(nid)
                .fetch_optional(&st.pool)
                .await
                .ok()
                .flatten()
                .map(|r| r.get::<String, _>("name"))
                .unwrap_or_default();
                let artist_id = sqlx::query("SELECT artist_id FROM albums WHERE id = ?")
                    .bind(nid)
                    .fetch_optional(&st.pool)
                    .await
                    .ok()
                    .flatten()
                    .map(|r| r.get::<i64, _>("artist_id"));
                let children = match virtual_album(&st, nid).await {
                    Ok(c) => c,
                    Err(e) => return error(0, &e.to_string()),
                };
                (children, name, artist_id.map(dirv_artist_id))
            }
            _ => return error(10, "invalid directory id"),
        };
        let mut directory = json!({"id": id, "name": name, "child": children});
        if let Some(p) = parent {
            directory.as_object_mut().unwrap().insert("parent".into(), json!(p));
        }
        return ok(json!({ "directory": directory })).into_response();
    }

    let Some((folder, prefix)) = parse_dir_id(id) else {
        return error(10, "invalid directory id");
    };

    let dirs = match child_dirs(&st, folder, &prefix).await {
        Ok(d) => d,
        Err(e) => return error(0, &e.to_string()),
    };
    let songs = match child_songs(&st, folder, &prefix).await {
        Ok(s) => s,
        Err(e) => return error(0, &e.to_string()),
    };

    let mut children: Vec<Value> = dirs
        .iter()
        .map(|d| {
            let full = if prefix.is_empty() {
                d.clone()
            } else {
                format!("{prefix}/{d}")
            };
            dir_child(folder, &prefix, &full, d)
        })
        .collect();
    children.extend(songs);

    let name = prefix
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| {
            st.cfg
                .music_dirs
                .get(folder as usize)
                .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
        })
        .unwrap_or_else(|| "Music".into());

    let mut directory = json!({
        "id": id,
        "name": name,
        "child": children,
    });
    if !prefix.is_empty() {
        let parent = prefix.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
        directory
            .as_object_mut()
            .unwrap()
            .insert("parent".into(), json!(dir_id(folder, parent)));
    }

    ok(json!({ "directory": directory })).into_response()
}
