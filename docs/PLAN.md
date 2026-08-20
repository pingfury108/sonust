# Sonust 设计方案

极简、高吞吐的 Rust 音乐流媒体服务端，遵循 OpenSubsonic 协议，无损透传（Direct Stream Only），单文件部署。

## 1. 设计目标

| 目标 | 指标 |
|---|---|
| OpenSubsonic 兼容 | Symfonium / Subtracks / Tempus 等标准客户端即连即用 |
| 资源占用 | 待机 RAM < 30MB，CPU 趋近 0 |
| 部署形态 | 单个静态二进制，无外部依赖（无 Docker/Redis/PG） |
| 转码策略 | 不转码，原文件流直出，客户端 `maxBitRate`/`format` 参数一律接受并忽略 |

## 2. 技术栈（定稿）

| 模块 | 选型 | 说明 |
|---|---|---|
| 运行时 | Tokio | 异步并发 |
| Web 框架 | Axum 0.8 + tower-http 0.6 | `ServeFile` 原生处理 Range / 206 Partial Content |
| 标签解析 | lofty（最新稳定版） | ID3v1/v2、Vorbis Comment、MP4 全覆盖 |
| 数据库 | sqlx + SQLite（WAL） | 唯一事实来源；运行时查询，不用编译期宏 |
| 序列化 | serde + serde_json | 仅 JSON；`f=xml` 请求也返回 JSON（现代客户端均显式 `f=json`） |
| 封面缩放 | image crate | `getCoverArt` 的 `size` 参数需要服务端缩放 |
| 目录遍历 | walkdir | 扫描 |
| 文件监听 | notify | Phase 3，增量实时刷新 |
| CLI | clap（derive） | 见 §4 |

## 3. 工程结构：lib + 薄 CLI

单一 crate，`src/lib.rs` 暴露全部能力，`src/main.rs` 只做参数解析和启动。逻辑全部在 lib，CLI 是壳，方便后续集成测试与复用。

```
sonust/
├── Cargo.toml
├── src/
│   ├── lib.rs              # 公开导出，run(config) 入口
│   ├── main.rs             # 薄 CLI：clap 解析 → Config → sonust::run()
│   ├── config.rs           # Config 结构与加载（flag > env > 默认值）
│   ├── error.rs            # 统一错误 → Subsonic 协议错误码
│   ├── auth/
│   │   ├── mod.rs          # Axum extractor：解析 u/t/s/apiKey/p 并校验
│   │   └── credentials.rs  # 凭证模型与校验逻辑（见 §6）
│   ├── db/
│   │   ├── mod.rs          # 连接池、迁移
│   │   └── models.rs       # Artist / Album / Track
│   ├── scanner/
│   │   ├── mod.rs          # walkdir 遍历 + 增量扫描（path+mtime+size 指纹）
│   │   └── tags.rs         # lofty 元数据提取
│   ├── cover.rs            # 封面提取、缩放、磁盘缓存
│   └── subsonic/
│       ├── mod.rs          # Router 构建（/rest/* 与 *.view 后缀归一化）
│       ├── response.rs     # subsonic-response 外壳（协议修正版，见 §7.1）
│       ├── system.rs       # ping / getLicense / getOpenSubsonicExtensions
│       ├── browsing.rs     # getMusicFolders / getArtists / getAlbum / getIndexes ...
│       ├── lists.rs        # getAlbumList2 / search3 / getRandomSongs / getGenres
│       └── media.rs        # stream / getCoverArt
└── docs/PLAN.md
```

## 4. CLI 与配置

### 4.1 命令行

```
sonust [OPTIONS] <COMMAND>

Commands:
  serve   启动服务（默认，可省略）
  scan    离线执行一次全量/增量扫描后退出
  user    用户管理（Phase 2：add / passwd / list）

Options:
  -m, --music-dir <PATH>   音乐库根目录（可多次指定多个目录）
      --data-dir <PATH>    数据目录（数据库 + 封面缓存）
                           [默认: ~/.local/share/sonust（dirs::data_dir）]
      --host <ADDR>        监听地址        [默认: 0.0.0.0]
  -p, --port <PORT>        监听端口        [默认: 4533，Navidrome 惯例]
  -u, --user <NAME>        初始用户名      [默认: admin]
      --password <PASS>    初始密码（不推荐，优先用环境变量）
  -v, --verbose            详细日志
```

对应环境变量（全部支持，前缀 `SONUST_`）：
`SONUST_MUSIC_DIR`、`SONUST_DATA_DIR`、`SONUST_PORT`、`SONUST_HOST`、`SONUST_USER`、`SONUST_PASSWORD`。

### 4.2 优先级

```
CLI flag  >  环境变量  >  内置默认值
```

MVP 不引入配置文件，保持零配置启动；确有需求时再加 `$DATA_DIR/config.toml`（插入在 env 与默认值之间）。

### 4.3 数据目录布局

```
$DATA_DIR/
├── sonust.db          # SQLite（WAL 模式）
└── covers/            # 封面缩略图缓存，{track/album id}_{size}.webp
```

## 5. 数据库 Schema（MVP）

```sql
CREATE TABLE artists (
    id    INTEGER PRIMARY KEY,
    name  TEXT NOT NULL UNIQUE
);
CREATE TABLE albums (
    id        INTEGER PRIMARY KEY,
    artist_id INTEGER NOT NULL REFERENCES artists(id),
    name      TEXT NOT NULL,
    year      INTEGER,
    UNIQUE(artist_id, name)
);
CREATE TABLE tracks (
    id         INTEGER PRIMARY KEY,
    album_id   INTEGER REFERENCES albums(id),
    artist_id  INTEGER REFERENCES artists(id),
    title      TEXT NOT NULL,
    track_no   INTEGER,
    duration   REAL,
    format     TEXT,             -- mp3/flac/m4a...
    path       TEXT NOT NULL UNIQUE,  -- 相对 music-dir 的路径
    mtime      INTEGER NOT NULL,      -- 增量扫描指纹
    size       INTEGER NOT NULL,
    has_cover  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_tracks_album  ON tracks(album_id);
CREATE INDEX idx_tracks_artist ON tracks(artist_id);
CREATE INDEX idx_tracks_title  ON tracks(title);
```

**ID 稳定性**：以 `path` 为唯一键 upsert，路径不变则 ID 不变——客户端本地缓存（收藏/歌单/队列）不会因重扫失效。这是硬性要求。

## 6. 认证设计

### 6.1 Subsonic 认证的三种方式与约束

| 方式 | 参数 | 服务端存储要求 | 评价 |
|---|---|---|---|
| apiKey | `apiKey=<key>` | 只需存 **key 的哈希**（SHA-256）比对 | OpenSubsonic 推荐，等同 bearer token，安全 |
| token+salt | `u`, `t=md5(password+salt)`, `s` | **必须能拿到明文密码**才能验算 | 协议历史包袱，安全性差 |
| 明文 | `p=<pass>` 或 `p=enc:<hex>` | 同上 |  deprecated，默认拒绝 |

关键结论：**t/s 校验在数学上要求服务端持有明文密码**（Navidrome 等实现同样如此）。因此设计如下：

### 6.2 Sonust 的取舍

1. **MVP 单用户**。密码只从 CLI flag / 环境变量注入，**仅驻留内存，永不落盘**。这样 t/s 可以验算，磁盘上零明文。
2. **apiKey 作为推荐方式**。首次启动时若未指定，自动生成一个随机 apiKey 打印到日志（一次），数据库存其 SHA-256。客户端配置 apiKey 后即使重启换密码也不受影响（key 独立于密码）。
3. **`p=` 明文认证默认拒绝**，返回协议错误 `code=41 (Token authentication not supported)` 的同类语义；加 `--allow-plaintext-auth` 才放行（供古董客户端，需用户显式知情）。
4. 所有认证都要求带 `u`（用户名），MVP 阶段只校验等于配置用户。
5. Phase 2 多用户：`users` 表存 `username + sha256(api_key)`；t/s 支持则按用户显式开启并打印警告。

### 6.3 传输安全

服务本身不实现 TLS（违背极简目标）。文档明确：**仅监听内网/本机，公网暴露必须套 Caddy/Nginx 反代 TLS**。因为 t/s 与 apiKey 都是可重放的查询参数，明文 HTTP 等于裸奔。

### 6.4 实现形态

- Axum 自定义 extractor `SubsonicAuth`：从 query 提取 `u/t/s/apiKey/p/v/c/f`，按 §6.2 顺序校验；
- 失败返回 HTTP 200 + `{"subsonic-response":{"status":"failed","error":{"code":40,"message":"Wrong username or password"}}}`（**Subsonic 错误一律走协议错误体，不打 HTTP 4xx/5xx**）；
- 未知查询参数一律忽略（客户端会附带 `maxBitRate`、`format`、`estimateContentLength` 等，透传策略下直接丢弃）。

### 6.5 无状态性

Subsonic 协议本身无 session——每个请求自带凭证。不需要 cookie/JWT/会话表，天然适合极简实现。

## 7. 协议层关键修正（相对初稿）

### 7.1 响应外壳

业务字段与 `status`/`version` **平级**，没有 `data` 层：

```json
{
  "subsonic-response": {
    "status": "ok",
    "version": "1.16.1",
    "type": "Sonust",
    "serverVersion": "0.1.0",
    "openSubsonic": true,
    "license": { "valid": true }
  }
}
```

serde 字段必须显式 rename：`#[serde(rename = "type")]`、`serverVersion`/`openSubsonic` 用 camelCase。`ping` 响应不带业务字段。

### 7.2 路由

`/rest/ping` 与 `/rest/ping.view` 等价：Axum fallback 统一剥 `.view` 后缀后分发，不逐个注册。

### 7.3 接口清单（初稿的 `getSongList` 不存在，以下为真实规范）

| 阶段 | 端点 |
|---|---|
| 系统 | `ping`、`getLicense`、`getOpenSubsonicExtensions` |
| 浏览（ID3 树） | `getMusicFolders`、`getArtists`、`getArtist`、`getAlbum` |
| 浏览（目录树，Phase 2） | `getIndexes`、`getMusicDirectory` |
| 列表 | `getAlbumList2`、`search3`、`getRandomSongs`、`getGenres` |
| 播放 | `stream`（Range）、`getCoverArt`（支持 `size`，缩略图落盘缓存） |
| Phase 3 | `star`/`unstar`/`getStarred2`、`scrobble`、`getPlaylists` |

## 8. 运行与扫描流程

```
sonust serve
  ├─ 加载 Config（flag > env > default）
  ├─ 打开 $DATA_DIR/sonust.db，执行迁移
  ├─ 启动时后台增量扫描（walkdir 遍历，path+mtime+size 指纹，变更文件才过 lofty）
  ├─ 封面：扫描时提取内嵌图 → image 缩放 2~3 档 → covers/ 落盘
  └─ Axum 服务：/rest/* 全部经 SubsonicAuth extractor
       ├─ 元数据类 → SQLite 查询 → JSON
       └─ stream/getCoverArt → ServeFile（Range/206 免费获得）
```

## 9. 路线图

- **Phase 1（MVP，已完成）**：lib 骨架 + CLI（§4）+ 响应外壳 + 单用户认证（§6.2）+ SQLite 扫描入库 + `ping`/`getLicense`/`getOpenSubsonicExtensions` + `getArtists`/`getAlbum`/`getAlbumList2`/`search3`/`getRandomSongs` + `stream`（Range）。
- **Phase 2（已完成）**：`getCoverArt`（含 size 缩放缓存，image crate）、目录树 `getIndexes`/`getMusicDirectory`、`getGenres`、`star`/`unstar`/`getStarred`/`getStarred2`、扫描删除清理。
- **Phase 3（部分完成）**：notify 实时监听（2s 防抖，已完成）；待做：`scrobble` 播放统计、`getPlaylists` 歌单、`user` 子命令与多用户。
