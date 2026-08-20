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

fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
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
