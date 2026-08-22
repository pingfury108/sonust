#!/usr/bin/env python3
"""Sonust OpenSubsonic 协议兼容性测试套件。

用法:
    python3 tests/opensubsonic_compliance.py \
        --base http://127.0.0.1:4533 --user admin --apikey testkey [--password secret]

覆盖:
    1. 响应外壳 schema（status/version/type/serverVersion/openSubsonic）
    2. 所有已实现端点的协议结构与必填字段（Child / AlbumID3 / ArtistID3）
    3. 鉴权矩阵（无凭证/错误 apiKey/t,s/p 明文）
    4. stream 的 Range / 206 语义
    5. star/unstar 回环
    6. .view 后缀等价性
"""
import argparse
import hashlib
import json
import sys
import urllib.error
import urllib.parse
import urllib.request

PASS, FAIL, SKIP = "PASS", "FAIL", "SKIP"
results = []  # (name, status, detail)


def report(name, status, detail=""):
    results.append((name, status, detail))
    mark = {PASS: "\033[32mPASS\033[0m", FAIL: "\033[31mFAIL\033[0m", SKIP: "\033[33mSKIP\033[0m"}[status]
    print(f"[{mark}] {name}" + (f"  -- {detail}" if detail else ""))


class Client:
    def __init__(self, base, user, apikey, password):
        self.base = base.rstrip("/")
        self.user = user
        self.apikey = apikey
        self.password = password

    def url(self, endpoint, params=None, auth=True):
        q = {"v": "1.16.1", "c": "compliance-test", "f": "json"}
        if auth:
            q["u"] = self.user
            q["apiKey"] = self.apikey
        if params:
            q.update(params)
        return f"{self.base}/rest/{endpoint}?{urllib.parse.urlencode(q)}"

    def get(self, endpoint, params=None, auth=True, raw=False):
        req = urllib.request.Request(self.url(endpoint, params, auth))
        try:
            with urllib.request.urlopen(req, timeout=15) as resp:
                body = resp.read()
                if raw:
                    return resp.status, dict(resp.headers), body
                return resp.status, dict(resp.headers), json.loads(body)
        except urllib.error.HTTPError as e:
            return e.code, {}, {}

    @staticmethod
    def payload(body):
        return body.get("subsonic-response", {})


WRAPPER_FIELDS = {"status", "version", "type", "serverVersion", "openSubsonic"}

# OpenSubsonic schema 必填字段（字段名: 期望类型）
CHILD_REQUIRED = {
    "id": str, "isDir": bool, "title": str, "contentType": str,
    "suffix": str, "duration": int, "size": int, "created": str,
    "mediaType": str, "type": str,
}
ALBUM_REQUIRED = {"id": str, "name": str, "songCount": int, "duration": int, "created": str}
ARTIST_REQUIRED = {"id": str, "name": str, "albumCount": int}


def check_fields(obj, required, label, errors):
    for field, typ in required.items():
        if field not in obj:
            errors.append(f"{label} 缺字段 {field}")
        elif obj[field] is not None and not isinstance(obj[field], typ):
            errors.append(f"{label} 字段 {field} 类型错误: {type(obj[field]).__name__}")


def expect_ok(name, body, top_key=None):
    p = Client.payload(body)
    if p.get("status") != "ok":
        report(name, FAIL, f"status={p.get('status')} error={p.get('error')}")
        return None
    missing = WRAPPER_FIELDS - set(p)
    if missing:
        report(name, FAIL, f"外壳缺字段 {missing}")
        return None
    if top_key and top_key not in p:
        report(name, FAIL, f"缺顶层键 {top_key}")
        return None
    report(name, PASS)
    return p


def expect_failed(name, body, code=None):
    p = Client.payload(body)
    if p.get("status") == "failed" and (code is None or p.get("error", {}).get("code") == code):
        report(name, PASS)
    else:
        report(name, FAIL, f"status={p.get('status')} error={p.get('error')}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://127.0.0.1:4533")
    ap.add_argument("--user", default="admin")
    ap.add_argument("--apikey", default="")
    ap.add_argument("--password", default="")
    args = ap.parse_args()
    c = Client(args.base, args.user, args.apikey, args.password)

    print("=" * 60)
    print("1. 鉴权矩阵")
    print("=" * 60)

    _, _, body = c.get("ping", auth=False)
    expect_failed("无凭证 -> 拒绝(code 10)", body, 10)

    _, _, body = c.get("ping", params={"u": args.user, "apiKey": "wrong-key"}, auth=False)
    expect_failed("错误 apiKey -> 拒绝(code 40)", body, 40)

    if args.password:
        _, _, body = c.get("ping", params={"u": args.user, "p": args.password}, auth=False)
        expect_failed("p= 明文 -> 默认拒绝(code 41)", body, 41)
        salt = "deadbeef"
        token = hashlib.md5((args.password + salt).encode()).hexdigest()
        _, _, body = c.get("ping", params={"u": args.user, "t": token, "s": salt}, auth=False)
        expect_ok("t/s token -> 通过", body)
    else:
        report("t/s token 认证", SKIP, "未提供 --password")

    _, _, body = c.get("ping")
    expect_ok("apiKey -> 通过", body)

    print("=" * 60)
    print("2. 系统端点")
    print("=" * 60)

    _, _, body = c.get("ping.view")
    expect_ok("ping.view 后缀等价", body)

    p = expect_ok("getLicense", c.get("getLicense")[2], "license")
    if p:
        lic = p["license"]
        report("getLicense.valid=true", PASS if lic.get("valid") is True else FAIL, str(lic))

    p = expect_ok("getOpenSubsonicExtensions", c.get("getOpenSubsonicExtensions")[2],
                  "openSubsonicExtensions")
    if p:
        report("openSubsonic 声明", PASS if Client.payload(c.get("ping")[2]).get("openSubsonic") is True else FAIL)

    print("=" * 60)
    print("3. 浏览端点")
    print("=" * 60)

    p = expect_ok("getMusicFolders", c.get("getMusicFolders")[2], "musicFolders")
    folders = (p or {}).get("musicFolders", {}).get("musicFolder", [])

    p = expect_ok("getArtists", c.get("getArtists")[2], "artists")
    artist_id = None
    if p:
        errors = []
        for idx in p["artists"].get("index", []):
            for a in idx.get("artist", []):
                check_fields(a, ARTIST_REQUIRED, f"ArtistID3({a.get('name')})", errors)
                artist_id = artist_id or a["id"]
        report("ArtistID3 schema", PASS if not errors else FAIL, "; ".join(errors[:3]))

    if artist_id:
        p = expect_ok("getArtist", c.get("getArtist", {"id": artist_id})[2], "artist")
    else:
        report("getArtist", SKIP, "库为空")

    album_id, song_id = None, None
    p = expect_ok("getAlbumList2(random)", c.get("getAlbumList2", {"type": "random", "size": 10})[2], "albumList2")
    if p:
        albums = p["albumList2"].get("album", [])
        errors = []
        for a in albums:
            check_fields(a, ALBUM_REQUIRED, f"AlbumID3({a.get('name')})", errors)
        report("AlbumID3 schema", PASS if not errors else FAIL, "; ".join(errors[:3]))
        album_id = albums[0]["id"] if albums else None

    for t in ["newest", "alphabeticalByName", "alphabeticalByArtist", "starred", "recent"]:
        _, _, body = c.get("getAlbumList2", {"type": t, "size": 5})
        expect_ok(f"getAlbumList2 type={t}", body, "albumList2")

    if album_id:
        p = expect_ok("getAlbum", c.get("getAlbum", {"id": album_id})[2], "album")
        if p:
            songs = p["album"].get("song", [])
            errors = []
            for s in songs:
                check_fields(s, CHILD_REQUIRED, f"Child({s.get('title')})", errors)
            report("Child schema (getAlbum)", PASS if not errors else FAIL, "; ".join(errors[:3]))
            song_id = songs[0]["id"] if songs else None
            if song_id:
                p2 = Client.payload(c.get("getSong", {"id": song_id})[2])
                report("getSong 单曲详情",
                       PASS if p2.get("song", {}).get("id") == song_id else FAIL,
                       p2.get("song", {}).get("title", ""))
    else:
        report("getAlbum", SKIP, "无专辑")

    p = expect_ok("getIndexes", c.get("getIndexes")[2], "indexes")
    dir_id = None
    if p:
        for idx in p["indexes"].get("index", []):
            for a in idx.get("artist", []):
                if str(a.get("id", "")).startswith("dir-"):
                    dir_id = a["id"]
                    break
    if dir_id:
        expect_ok("getMusicDirectory", c.get("getMusicDirectory", {"id": dir_id})[2], "directory")
    else:
        _, _, body = c.get("getMusicDirectory", {"id": "dir-0-"})
        expect_ok("getMusicDirectory(根)", body, "directory")

    print("=" * 60)
    print("4. 列表与搜索端点")
    print("=" * 60)

    p = expect_ok("search3 空查询=全库", c.get("search3", {
        "query": '""', "songCount": 500, "albumCount": 500, "artistCount": 500})[2], "searchResult3")
    total_songs = 0
    if p:
        r = p["searchResult3"]
        total_songs = len(r.get("song", []))
        errors = []
        for s in r.get("song", []):
            check_fields(s, CHILD_REQUIRED, f"Child({s.get('title')})", errors)
        report(f"Child schema (search3, {total_songs} 首)", PASS if not errors else FAIL,
               "; ".join(errors[:3]))
        song_id = song_id or (r["song"][0]["id"] if r.get("song") else None)

    p1 = Client.payload(c.get("search3", {"query": '""', "songCount": 2, "songOffset": 0,
                                          "artistCount": 0, "albumCount": 0})[2])
    p2 = Client.payload(c.get("search3", {"query": '""', "songCount": 2, "songOffset": 2,
                                          "artistCount": 0, "albumCount": 0})[2])
    ids1 = {s["id"] for s in p1.get("searchResult3", {}).get("song", [])}
    ids2 = {s["id"] for s in p2.get("searchResult3", {}).get("song", [])}
    report("search3 分页不重叠", PASS if not (ids1 & ids2) else FAIL, f"重叠 {ids1 & ids2}")

    expect_ok("getRandomSongs", c.get("getRandomSongs", {"size": 5})[2], "randomSongs")
    expect_ok("getGenres", c.get("getGenres")[2], "genres")
    genres = Client.payload(c.get("getGenres")[2]).get("genres", {}).get("genre", [])
    if genres:
        expect_ok("getSongsByGenre", c.get("getSongsByGenre", {"genre": genres[0]["value"]})[2],
                  "songsByGenre")
    else:
        report("getSongsByGenre", SKIP, "库中无 genre 标签")

    print("=" * 60)
    print("5. 收藏 / 书签 / 队列")
    print("=" * 60)

    if song_id:
        c.get("star", {"id": song_id})
        p = Client.payload(c.get("getStarred2")[2])
        starred_ids = {s["id"] for s in p.get("starred2", {}).get("song", [])}
        ok1 = song_id in starred_ids
        c.get("unstar", {"id": song_id})
        p = Client.payload(c.get("getStarred2")[2])
        starred_ids = {s["id"] for s in p.get("starred2", {}).get("song", [])}
        ok2 = song_id not in starred_ids
        report("star -> getStarred2 -> unstar 回环", PASS if ok1 and ok2 else FAIL)
    else:
        report("star/unstar 回环", SKIP, "无曲目")

    expect_ok("getStarred2", c.get("getStarred2")[2], "starred2")
    expect_ok("getBookmarks", c.get("getBookmarks")[2], "bookmarks")
    expect_ok("createBookmark", c.get("createBookmark", {"id": song_id or "tr-1", "position": 1000})[2])
    expect_ok("deleteBookmark", c.get("deleteBookmark", {"id": song_id or "tr-1"})[2])
    expect_ok("getPlayQueue", c.get("getPlayQueue")[2], "playQueue")
    expect_ok("savePlayQueue", c.get("savePlayQueue", {"id": song_id or "tr-1"})[2])
    expect_ok("getPlaylists", c.get("getPlaylists")[2], "playlists")
    expect_ok("getTopSongs", c.get("getTopSongs", {"artist": "test"})[2], "topSongs")
    expect_ok("getSimilarSongs2", c.get("getSimilarSongs2", {"id": song_id or "tr-1"})[2], "similarSongs2")
    expect_ok("getArtistInfo2", c.get("getArtistInfo2", {"id": artist_id or "ar-1"})[2], "artistInfo2")

    for ep, key in [("getPodcasts", "podcasts"), ("getInternetRadioStations", "internetRadioStations"),
                    ("getShares", "shares"), ("getVideos", "videos"), ("getAlbumInfo2", "albumInfo2")]:
        expect_ok(f"{ep}(桩)", c.get(ep)[2], key)

    print("=" * 60)
    print("5.5 歌单 CRUD")
    print("=" * 60)

    if song_id:
        pl_id = None
        p = Client.payload(c.get("createPlaylist", {"name": "compliance-test", "songId": song_id})[2])
        pl = p.get("playlist", {})
        if pl.get("id") and len(pl.get("entry", [])) == 1:
            pl_id = pl["id"]
            report("createPlaylist", PASS, pl_id)
        else:
            report("createPlaylist", FAIL, str(pl)[:100])

        if pl_id:
            p = Client.payload(c.get("getPlaylist", {"id": pl_id})[2])
            report("getPlaylist 条目正确",
                   PASS if p.get("playlist", {}).get("entry", [{}])[0].get("id") == song_id else FAIL)

            c.get("updatePlaylist", {"playlistId": pl_id, "songIndexToRemove": 0})
            p = Client.payload(c.get("getPlaylist", {"id": pl_id})[2])
            ok_rm = len(p.get("playlist", {}).get("entry", [])) == 0
            c.get("updatePlaylist", {"playlistId": pl_id, "songIdToAdd": song_id})
            p = Client.payload(c.get("getPlaylist", {"id": pl_id})[2])
            ok_add = len(p.get("playlist", {}).get("entry", [])) == 1
            report("updatePlaylist 删歌/加歌", PASS if ok_rm and ok_add else FAIL,
                   f"删后={ok_rm} 加后={ok_add}")

            c.get("deletePlaylist", {"id": pl_id})
            p = Client.payload(c.get("getPlaylist", {"id": pl_id})[2])
            report("deletePlaylist", PASS if p.get("status") == "failed" else FAIL)
        else:
            report("getPlaylist/updatePlaylist/deletePlaylist", SKIP, "创建失败")
    else:
        report("歌单系列", SKIP, "无曲目")

    if song_id:
        expect_ok("scrobble", c.get("scrobble", {"id": song_id, "submission": "true"})[2])
        p = Client.payload(c.get("getAlbumList2", {"type": "frequent", "size": 5})[2])
        albums = p.get("albumList2", {}).get("album", [])
        report("frequent 有真实播放统计", PASS if albums and albums[0].get("playCount", 0) > 0 else FAIL,
               f"{len(albums)} 张专辑")
        expect_ok("scrobble submission=false(正在播放)",
                  c.get("scrobble", {"id": song_id, "submission": "false"})[2])
    else:
        report("scrobble", SKIP, "无曲目")

    print("=" * 60)
    print("6. 流式传输")
    print("=" * 60)

    if song_id:
        status, headers, _ = c.get("stream", {"id": song_id}, raw=True)
        report("stream 全量 200", PASS if status == 200 else FAIL, f"status={status}")
        report("stream accept-ranges", PASS if "bytes" in headers.get("Accept-Ranges", headers.get("accept-ranges", "")) else FAIL,
               headers.get("Accept-Ranges", headers.get("accept-ranges", "无")))

        req = urllib.request.Request(c.url("stream", {"id": song_id}),
                                     headers={"Range": "bytes=0-1023"})
        try:
            with urllib.request.urlopen(req, timeout=15) as resp:
                n = len(resp.read())
                cr = resp.headers.get("Content-Range", "")
                report("stream Range -> 206", PASS if resp.status == 206 else FAIL, f"status={resp.status}")
                report("stream Content-Range 正确", PASS if cr.startswith("bytes 0-1023/") else FAIL, cr)
                report("stream Range 返回 1024 字节", PASS if n == 1024 else FAIL, f"{n} bytes")
        except urllib.error.HTTPError as e:
            report("stream Range -> 206", FAIL, f"status={e.code}")

        _, _, body = c.get("stream", {"id": "tr-999999"})
        expect_failed("stream 不存在 id -> code 70", body, 70)
    else:
        report("stream 系列", SKIP, "无曲目")

    if song_id:
        status, headers, _ = c.get("getCoverArt", {"id": song_id}, raw=True)
        if status == 200:
            report("getCoverArt 200", PASS, headers.get("Content-Type", ""))
            status2, _, body2 = c.get("getCoverArt", {"id": song_id, "size": 300}, raw=True)
            report("getCoverArt size=300", PASS if status2 == 200 else FAIL, f"status={status2}")
        else:
            report("getCoverArt", SKIP, "该曲目无封面")
        _, _, body = c.get("getCoverArt", {"id": "tr-999999"})
        expect_failed("getCoverArt 不存在 id -> code 70", body, 70)

    print("=" * 60)
    print("7. 歌词")
    print("=" * 60)

    if song_id:
        p = expect_ok("getLyricsBySongId", c.get("getLyricsBySongId", {"id": song_id})[2], "lyricsList")
        if p:
            sl = p["lyricsList"].get("structuredLyrics", [])
            if sl:
                lines = sl[0].get("line", [])
                has_ts = any("start" in l for l in lines)
                report("结构化歌词内容", PASS if lines else FAIL,
                       f"{len(lines)} 行, synced={sl[0].get('synced')}, 有时间轴={has_ts}")
            else:
                report("结构化歌词内容", SKIP, "该曲目无歌词")
        expect_ok("getLyrics", c.get("getLyrics", {"title": "", "artist": ""})[2], "lyrics")
    else:
        report("歌词系列", SKIP, "无曲目")

    print("=" * 60)
    print("8. 其他")
    print("=" * 60)

    _, _, body = c.get("nonExistentEndpoint")
    expect_failed("未知端点 -> failed", body)

    print()
    print("=" * 60)
    npass = sum(1 for _, s, _ in results if s == PASS)
    nfail = sum(1 for _, s, _ in results if s == FAIL)
    nskip = sum(1 for _, s, _ in results if s == SKIP)
    print(f"结果: {npass} PASS / {nfail} FAIL / {nskip} SKIP")
    print()
    print("已知未实现端点（不在本次检测范围，按计划排期）:")
    for ep in ["getAlbumList/search2 等 v1 老接口", "download", "jukebox", "多用户管理"]:
        print(f"  - {ep}")
    print("=" * 60)
    sys.exit(1 if nfail else 0)


if __name__ == "__main__":
    main()
