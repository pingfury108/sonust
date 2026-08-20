# Sonust

极简、高吞吐的 Rust 音乐流媒体服务端，遵循 OpenSubsonic 协议。单静态二进制，无损透传（Direct Stream Only），待机内存 ~17MB。

Symfonium / Tempus / Subtracks / Feishin 等任意标准 Subsonic 客户端即连即用。

## 特性

- **OpenSubsonic 兼容**：35+ 个 `/rest` 端点，协议符合性测试套件 66 PASS
- **无损透传**：不做服务端转码，`tower-http ServeFile` 原生支持 HTTP Range / 206（拖动进度条、断点续传）
- **自动曲库**：walkdir 全量扫描 + mtime/size 指纹增量 + notify 实时监听（加歌/删歌秒级入库，无需重启）
- **元数据**：lofty 解析 ID3v1/v2 / Vorbis Comment / MP4 标签（MP3/FLAC/M4A/WAV/OGG/OPUS…）
- **歌词**：内嵌歌词 → 同目录 .lrc → LRCLIB 在线查询（磁盘缓存）三级回退，支持滚动歌词时间轴
- **封面**：内嵌封面提取 + 磁盘缓存 + `size` 参数服务端缩放
- **收藏/歌单/播放统计**：star、playlist CRUD（多设备同步）、scrobble 播放记录（frequent/recent 真实排序）
- **去重工具**：`dupes` 子命令，字节级哈希 + 标签/时长聚类两级检测，报告 + 隔离
- **安全认证**：apiKey（推荐，服务端只存哈希）+ t/s token；密码仅驻留内存永不落盘；`p=` 明文认证默认拒绝

## 快速开始

```bash
# 需要 Rust 工具链（mise.toml 已声明 rust = "stable"）
cargo build --release

sonust \
  --music-dir /path/to/music \   # 可多次指定多个音乐库
  --data-dir  /path/to/data  \   # 默认 ~/.local/share/sonust
  --port 4533 \
  serve
```

凭据通过环境变量注入（密码仅存内存）：

```bash
export SONUST_PASSWORD=your-password    # t/s token 认证用
export SONUST_API_KEY=your-api-key      # 可选；不设置则首次启动自动生成并打印一次
```

### 客户端连接

| 字段 | 值 |
|---|---|
| 服务器 | `http://<host>:4533` |
| 用户名 | `admin`（`--user` 可改） |
| 密码 | `SONUST_PASSWORD` 的值（客户端自动走 t/s token） |
| 或 API Key | `SONUST_API_KEY` 的值（推荐，密码框留空） |

> 服务不内置 TLS。仅限内网使用；公网暴露请套 Caddy/Nginx 反代 HTTPS。

## CLI

```
sonust [OPTIONS] <COMMAND>

Commands:
  serve   启动服务（默认）
  scan    执行一次扫描后退出
  dupes   重复歌曲检测报告；--quarantine <目录> 隔离非保留副本（保留最优版本）

Options:
  -m, --music-dir <PATH>   音乐库根目录（可多次）
      --data-dir <PATH>    数据目录（sonust.db + covers/ + lyrics/）
      --host <ADDR>        [默认: 0.0.0.0]
  -p, --port <PORT>        [默认: 4533]
  -u, --user <NAME>        [默认: admin]
      --password <PASS>    密码（建议用 SONUST_PASSWORD 环境变量）
      --allow-plaintext-auth  允许 p= 明文认证（不安全，默认拒绝）
  -v, --verbose            详细日志
```

所有选项均有 `SONUST_*` 环境变量对应，优先级：**flag > env > 默认值**。

## 已实现的 Subsonic API

| 类别 | 端点 |
|---|---|
| 系统 | `ping` `getLicense` `getOpenSubsonicExtensions` |
| 浏览(ID3) | `getMusicFolders` `getArtists` `getArtist` `getAlbum` `getGenres` |
| 浏览(目录) | `getIndexes` `getMusicDirectory` |
| 列表/搜索 | `getAlbumList2`(random/newest/starred/frequent/recent/字母序) `search3` `getRandomSongs` `getSongsByGenre` `getTopSongs` `getSimilarSongs2` `getArtistInfo2` |
| 播放 | `stream`(Range/206) `getCoverArt`(缩放) `download=stream` |
| 收藏 | `star` `unstar` `getStarred` `getStarred2` |
| 歌单 | `getPlaylists` `getPlaylist` `createPlaylist` `updatePlaylist` `deletePlaylist` |
| 歌词 | `getLyrics` `getLyricsBySongId`（声明 `songLyrics` 扩展） |
| 统计 | `scrobble`（批量 id/time，submission=false 不落库） |
| 书签/队列 | `getBookmarks` `createBookmark` `deleteBookmark` `getPlayQueue` `savePlayQueue`（暂不持久化） |
| 空结果桩 | `getPodcasts` `getNewestPodcasts` `getInternetRadioStations` `getShares` `getVideos` `getChatMessages` `getAlbumInfo2` |

所有端点同时支持 `/rest/x` 与 `/rest/x.view` 两种形态；JSON 响应。

## 架构

```
src/
├── main.rs            # 薄 CLI 壳
├── lib.rs             # run(): 初始化 -> 后台扫描 + 文件监听 -> Axum 服务
├── cli.rs / config.rs # clap CLI 与配置
├── auth.rs            # SubsonicAuth extractor（apiKey / t,s / p）
├── db.rs              # sqlx + SQLite (WAL, busy_timeout)
├── scanner.rs         # 扫描 + 增量 + notify 监听（防抖限频）
├── cover.rs           # 封面提取/缩放/缓存
├── lyrics.rs          # LRC 解析 + LRCLIB 查询
├── dupes.rs           # 重复检测与隔离
└── subsonic/          # 协议层：统一响应外壳 + 按域分模块的 handler
```

关键设计：

- **ID 稳定**：以 `(folder, path)` 为唯一键 upsert，重扫不失效客户端缓存
- **SQLite 单一事实来源**：不整库驻留内存，待机 ~17MB
- **错误协议化**：失败一律 HTTP 200 + `{"subsonic-response":{"status":"failed","error":{code,message}}}`
- **无状态**：协议自带凭证，无 session/JWT

## 构建与部署

```bash
# musl 静态二进制（零 glibc 依赖，~9MB）
cargo build --release --target x86_64-unknown-linux-musl
strip target/x86_64-unknown-linux-musl/release/sonust
```

## 测试

```bash
# 对着运行中的实例跑协议符合性套件（66 项）
python3 tests/opensubsonic_compliance.py \
  --base http://127.0.0.1:4533 --user admin \
  --apikey <key> --password <pass>
```

覆盖：鉴权矩阵、响应外壳 schema、Child/AlbumID3/ArtistID3 必填字段、
分页、star/歌单/scrobble 回环、Range 语义、歌词结构、`.view` 后缀。

## 路线图

- [x] Phase 1：MVP（扫描/浏览/搜索/流式/认证）
- [x] Phase 2：目录树、封面缩放、收藏、增量与删除清理
- [x] Phase 3：notify 实时监听、scrobble、歌词（本地+LRCLIB）、歌单 CRUD、dupes 去重
- [ ] 多用户、书签/播放队列持久化、v1 老接口（search2/getAlbumList）

详细设计与协议修正说明见 [docs/PLAN.md](docs/PLAN.md)。
