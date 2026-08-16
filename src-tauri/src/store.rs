//! 剪贴板历史存储：基于 rusqlite（SQLite 内嵌）的单表存储。
//!
//! 表结构：
//! items(id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT, content TEXT NULL,
//!       image_path TEXT NULL, hash TEXT NOT NULL UNIQUE,
//!       pinned INTEGER DEFAULT 0, created_at INTEGER,
//!       source_app TEXT NULL, source_title TEXT NULL)
//! snippets(id INTEGER PRIMARY KEY AUTOINCREMENT, content TEXT NOT NULL,
//!          title TEXT NULL, group_name TEXT NOT NULL DEFAULT '默认',
//!          created_at INTEGER NOT NULL)

use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::clipboard::now_ms;

/// 默认分组名（删除分组时组内片段移入该组）
pub const DEFAULT_GROUP: &str = "默认";

/// 对外暴露的剪贴板条目
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: i64,
    pub kind: String,
    pub content: Option<String>,
    pub image_path: Option<String>,
    pub pinned: bool,
    pub created_at: i64,
    /// 来源应用 exe 名（如 chrome.exe），获取失败时为 None
    pub source_app: Option<String>,
    /// 来源窗口标题，获取失败时为 None
    pub source_title: Option<String>,
}

/// 对外暴露的常用片段
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snippet {
    pub id: i64,
    pub content: String,
    pub title: Option<String>,
    pub group_name: String,
    pub created_at: i64,
}

/// 存储层统一错误类型
pub type StoreError = Box<dyn Error + Send + Sync>;

/// 剪贴板历史存储
pub struct Store {
    conn: Connection,
    data_dir: PathBuf,
    /// 历史条数上限（可调，来自设置），超出后清理最旧非置顶条目
    max_items: i64,
}

impl Store {
    /// 打开（或创建）数据目录下的 SQLite 数据库，并确保表结构与图片目录存在。
    /// 旧版本数据库（0.2.0 无来源列）会自动迁移补列。
    pub fn new(data_dir: &Path) -> Result<Self, StoreError> {
        fs::create_dir_all(data_dir)?;
        fs::create_dir_all(data_dir.join("images"))?;
        let conn = Connection::open(data_dir.join("cliplite.db"))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS items (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                kind TEXT NOT NULL,
                content TEXT,
                image_path TEXT,
                hash TEXT NOT NULL UNIQUE,
                pinned INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL
            );",
        )?;
        migrate_columns(&conn)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS snippets (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                content TEXT NOT NULL,
                title TEXT,
                group_name TEXT NOT NULL DEFAULT '默认',
                created_at INTEGER NOT NULL
            );",
        )?;
        // 分组独立表：分组可先于片段存在（空分组也保留），默认组始终存在
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS snippet_groups (
                name TEXT PRIMARY KEY
            );",
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO snippet_groups (name) VALUES (?1)",
            params![DEFAULT_GROUP],
        )?;
        Ok(Self {
            conn,
            data_dir: data_dir.to_path_buf(),
            max_items: crate::settings::DEFAULT_MAX_ITEMS,
        })
    }

    /// 更新历史条数上限（来自设置），立即生效：调低上限时立刻清理超限条目
    pub fn set_max_items(&mut self, max: i64) {
        self.max_items = max.max(crate::settings::MAX_ITEMS_MIN);
        let _ = self.trim();
    }

    /// 图片文件存放目录
    pub fn images_dir(&self) -> PathBuf {
        self.data_dir.join("images")
    }

    /// 插入一条文本记录。
    /// 内容已存在时（按 hash 查重）仅刷新 created_at 并返回 `Ok(None)` 表示未新增；
    /// 否则插入并返回新条目。`source` 为来源信息（exe 名 / 窗口标题），可为空。
    pub fn insert_text(
        &self,
        content: &str,
        source: Option<(&str, &str)>,
    ) -> Result<Option<Item>, StoreError> {
        if content.trim().is_empty() {
            return Ok(None);
        }
        let hash = hash_bytes(content.as_bytes());
        let now = now_ms() as i64;
        if self.touch_existing(&hash, now)? {
            return Ok(None);
        }
        self.conn.execute(
            "INSERT INTO items (kind, content, image_path, hash, pinned, created_at, source_app, source_title)
             VALUES ('text', ?1, NULL, ?2, 0, ?3, ?4, ?5)",
            params![content, hash, now, source.map(|s| s.0), source.map(|s| s.1)],
        )?;
        let id = self.conn.last_insert_rowid();
        self.trim()?;
        Ok(Some(self.get_item(id)?))
    }

    /// 插入一条图片记录。`file_path` 为待入库的图片文件（调用方写入的临时文件）。
    /// 内部按文件内容哈希查重：重复时删除临时文件并返回 `Ok(None)`；
    /// 否则把文件重命名为 `<hash>.png` 放入图片目录，再插入记录。
    pub fn insert_image(
        &self,
        file_path: &Path,
        source: Option<(&str, &str)>,
    ) -> Result<Option<Item>, StoreError> {
        let bytes = fs::read(file_path)?;
        let hash = hash_bytes(&bytes);
        let now = now_ms() as i64;
        if self.touch_existing(&hash, now)? {
            let _ = fs::remove_file(file_path);
            return Ok(None);
        }
        let target = self.images_dir().join(format!("{hash}.png"));
        fs::rename(file_path, &target)?;
        self.conn.execute(
            "INSERT INTO items (kind, content, image_path, hash, pinned, created_at, source_app, source_title)
             VALUES ('image', NULL, ?1, ?2, 0, ?3, ?4, ?5)",
            params![
                target.to_string_lossy().to_string(),
                hash,
                now,
                source.map(|s| s.0),
                source.map(|s| s.1)
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        self.trim()?;
        Ok(Some(self.get_item(id)?))
    }

    /// 插入一条文件记录（复制文件/文件夹时，只记录路径列表，不读内容）。
    /// 路径列表以换行分隔存入 content；按路径列表哈希去重。
    pub fn insert_files(
        &self,
        paths: &[PathBuf],
        source: Option<(&str, &str)>,
    ) -> Result<Option<Item>, StoreError> {
        if paths.is_empty() {
            return Ok(None);
        }
        let joined: Vec<String> = paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        let content = joined.join("\n");
        let hash = hash_bytes(content.as_bytes());
        let now = now_ms() as i64;
        if self.touch_existing(&hash, now)? {
            return Ok(None);
        }
        self.conn.execute(
            "INSERT INTO items (kind, content, image_path, hash, pinned, created_at, source_app, source_title)
             VALUES ('file', ?1, NULL, ?2, 0, ?3, ?4, ?5)",
            params![content, hash, now, source.map(|s| s.0), source.map(|s| s.1)],
        )?;
        let id = self.conn.last_insert_rowid();
        self.trim()?;
        Ok(Some(self.get_item(id)?))
    }

    /// 查询历史：`query` 非空时按内容模糊搜索（LIKE，转义通配符），否则返回全部。
    /// 排序：置顶优先，其次按创建时间倒序；最多返回 max_items 条。
    pub fn list(&self, query: Option<&str>) -> Result<Vec<Item>, StoreError> {
        let q = query.map(str::trim).filter(|s| !s.is_empty());
        let mut items = Vec::new();
        let sql = match q {
            Some(_) => {
                "SELECT id, kind, content, image_path, pinned, created_at, source_app, source_title FROM items
                 WHERE content LIKE ?1 ESCAPE '\\'
                 ORDER BY pinned DESC, created_at DESC LIMIT ?2"
            }
            None => {
                "SELECT id, kind, content, image_path, pinned, created_at, source_app, source_title FROM items
                 ORDER BY pinned DESC, created_at DESC LIMIT ?1"
            }
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = match q {
            Some(q) => stmt.query_map(
                params![format!("%{}%", escape_like(q)), self.max_items],
                row_to_item,
            )?,
            None => stmt.query_map(params![self.max_items], row_to_item)?,
        };
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    /// 切换条目的置顶状态，返回更新后的条目。
    pub fn toggle_pin(&self, id: i64) -> Result<Item, StoreError> {
        let affected = self
            .conn
            .execute("UPDATE items SET pinned = 1 - pinned WHERE id = ?1", params![id])?;
        if affected == 0 {
            return Err(io::Error::new(io::ErrorKind::NotFound, "条目不存在").into());
        }
        self.get_item(id)
    }

    /// 删除指定条目（同时删除对应的图片文件）。
    pub fn delete(&self, id: i64) -> Result<(), StoreError> {
        if let Some(item) = self.get_item_opt(id)? {
            if let Some(path) = item.image_path {
                let _ = fs::remove_file(path);
            }
            self.conn.execute("DELETE FROM items WHERE id = ?1", params![id])?;
        }
        Ok(())
    }

    /// 清空全部历史并删除所有图片文件。
    pub fn clear(&self) -> Result<(), StoreError> {
        let paths: Vec<String> = {
            let mut stmt =
                self.conn
                    .prepare("SELECT image_path FROM items WHERE image_path IS NOT NULL")?;
            let rows = stmt.query_map([], |r| r.get::<_, Option<String>>(0))?;
            rows.filter_map(Result::ok).flatten().collect()
        };
        for path in paths {
            let _ = fs::remove_file(path);
        }
        self.conn.execute("DELETE FROM items", [])?;
        Ok(())
    }

    /// 按 id 获取条目，不存在时报错。
    pub fn get_item(&self, id: i64) -> Result<Item, StoreError> {
        self.get_item_opt(id)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "条目不存在").into()
        })
    }

    fn get_item_opt(&self, id: i64) -> Result<Option<Item>, StoreError> {
        self.conn
            .query_row(
                "SELECT id, kind, content, image_path, pinned, created_at, source_app, source_title
                 FROM items WHERE id = ?1",
                params![id],
                row_to_item,
            )
            .optional()
            .map_err(Into::into)
    }

    /// hash 已存在时仅刷新 created_at（去重 + 顺带把该条提到顶部）
    fn touch_existing(&self, hash: &str, now: i64) -> Result<bool, StoreError> {
        let affected = self.conn.execute(
            "UPDATE items SET created_at = ?2 WHERE hash = ?1",
            params![hash, now],
        )?;
        Ok(affected > 0)
    }

    /// 总数超过上限时，按 created_at 升序删除最旧的非置顶条目，直到达标。
    fn trim(&self) -> Result<(), StoreError> {
        let total: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))?;
        if total <= self.max_items {
            return Ok(());
        }
        let excess = total - self.max_items;
        let mut stmt = self.conn.prepare(
            "SELECT id FROM items WHERE pinned = 0 ORDER BY created_at ASC LIMIT ?1",
        )?;
        let ids: Vec<i64> = stmt
            .query_map(params![excess], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for id in ids {
            self.delete(id)?;
        }
        Ok(())
    }

    // ---------- 片段库 ----------

    /// 查询全部片段，按分组名 + 创建时间倒序
    pub fn list_snippets(&self) -> Result<Vec<Snippet>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, title, group_name, created_at FROM snippets
             ORDER BY group_name, created_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_snippet)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 新增片段；分组名默认「默认」，不允许重名片段（同内容视为重复，返回错误）
    pub fn add_snippet(
        &self,
        content: &str,
        title: Option<&str>,
        group_name: &str,
    ) -> Result<Snippet, StoreError> {
        let content = content.trim();
        if content.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "片段内容为空").into());
        }
        let group = normalize_group(group_name);
        self.ensure_group(&group)?;
        self.conn.execute(
            "INSERT INTO snippets (content, title, group_name, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![content, title, group, now_ms() as i64],
        )?;
        let id = self.conn.last_insert_rowid();
        self.get_snippet(id)
    }

    /// 更新片段内容/标题/分组；字段为 None 时保持不变
    pub fn update_snippet(
        &self,
        id: i64,
        content: Option<&str>,
        title: Option<&str>,
        group_name: Option<&str>,
    ) -> Result<Snippet, StoreError> {
        let cur = self.get_snippet(id)?;
        let new_content = content.map(str::trim).filter(|s| !s.is_empty());
        let new_title = title.map(str::trim).filter(|s| !s.is_empty());
        let new_group = group_name.map(normalize_group);
        if let Some(g) = new_group.as_deref() {
            self.ensure_group(g)?;
        }
        self.conn.execute(
            "UPDATE snippets SET content = ?1, title = ?2, group_name = ?3 WHERE id = ?4",
            params![
                new_content.unwrap_or(&cur.content),
                new_title.or(cur.title.as_deref()),
                new_group.as_deref().unwrap_or(&cur.group_name),
                id
            ],
        )?;
        self.get_snippet(id)
    }

    /// 删除片段
    pub fn delete_snippet(&self, id: i64) -> Result<(), StoreError> {
        self.conn
            .execute("DELETE FROM snippets WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// 新建分组（空分组也允许，独立持久化）；重名返回错误
    pub fn add_group(&self, name: &str) -> Result<(), StoreError> {
        let name = normalize_group(name);
        if self.group_exists(&name)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("分组「{name}」已存在"),
            )
            .into());
        }
        self.conn.execute(
            "INSERT INTO snippet_groups (name) VALUES (?1)",
            params![name],
        )?;
        Ok(())
    }

    /// 查询全部分组名（独立表，含空分组）
    pub fn list_groups(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT name FROM snippet_groups ORDER BY name")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 分组重命名；目标名已存在时返回错误
    pub fn rename_group(&self, old_name: &str, new_name: &str) -> Result<(), StoreError> {
        let new_name = normalize_group(new_name);
        if old_name == new_name {
            return Ok(());
        }
        if self.group_exists(&new_name)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("分组「{new_name}」已存在"),
            )
            .into());
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE snippets SET group_name = ?1 WHERE group_name = ?2",
            params![new_name, old_name],
        )?;
        tx.execute(
            "UPDATE snippet_groups SET name = ?1 WHERE name = ?2",
            params![new_name, old_name],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// 删除分组：组内片段移入「默认」组（不丢数据）；「默认」组本身不可删。
    /// 返回受影响（被迁移）的片段数。
    pub fn delete_group(&self, name: &str) -> Result<usize, StoreError> {
        if name == DEFAULT_GROUP {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "默认分组不可删除",
            )
            .into());
        }
        let tx = self.conn.unchecked_transaction()?;
        let affected = tx.execute(
            "UPDATE snippets SET group_name = ?1 WHERE group_name = ?2",
            params![DEFAULT_GROUP, name],
        )?;
        tx.execute("DELETE FROM snippet_groups WHERE name = ?1", params![name])?;
        tx.commit()?;
        Ok(affected)
    }

    /// 分组是否已存在（独立表查询）
    fn group_exists(&self, name: &str) -> Result<bool, StoreError> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM snippet_groups WHERE name = ?1",
            params![name],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// 确保分组存在于独立表（新增/移动片段到某分组时调用）
    fn ensure_group(&self, name: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO snippet_groups (name) VALUES (?1)",
            params![name],
        )?;
        Ok(())
    }

    fn get_snippet(&self, id: i64) -> Result<Snippet, StoreError> {
        self.conn
            .query_row(
                "SELECT id, content, title, group_name, created_at FROM snippets WHERE id = ?1",
                params![id],
                row_to_snippet,
            )
            .map_err(Into::into)
    }
}

/// 分组名规范化：去首尾空白，空名回退「默认」
fn normalize_group(name: &str) -> String {
    let t = name.trim();
    if t.is_empty() {
        DEFAULT_GROUP.to_string()
    } else {
        t.to_string()
    }
}

/// 查询行 → Snippet 映射
fn row_to_snippet(row: &rusqlite::Row) -> rusqlite::Result<Snippet> {
    Ok(Snippet {
        id: row.get(0)?,
        content: row.get(1)?,
        title: row.get(2)?,
        group_name: row.get(3)?,
        created_at: row.get(4)?,
    })
}

/// 计算数据的 SHA-256 十六进制摘要
pub fn hash_bytes(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// 转义 LIKE 模式中的特殊字符（% _ \），配合 `ESCAPE '\'` 使用
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// 查询行 → Item 映射
fn row_to_item(row: &rusqlite::Row) -> rusqlite::Result<Item> {
    Ok(Item {
        id: row.get(0)?,
        kind: row.get(1)?,
        content: row.get(2)?,
        image_path: row.get(3)?,
        pinned: row.get::<_, i64>(4)? != 0,
        created_at: row.get(5)?,
        source_app: row.get(6)?,
        source_title: row.get(7)?,
    })
}

/// 旧版本数据库迁移：为 items 表补齐来源列（列已存在时跳过，幂等）。
fn migrate_columns(conn: &Connection) -> Result<(), StoreError> {
    let has_column = |conn: &Connection, name: &str| -> Result<bool, StoreError> {
        let mut stmt = conn.prepare("PRAGMA table_info(items)")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        for row in rows {
            if row? == name {
                return Ok(true);
            }
        }
        Ok(false)
    };
    for col in ["source_app", "source_title"] {
        if !has_column(conn, col)? {
            conn.execute_batch(&format!("ALTER TABLE items ADD COLUMN {col} TEXT;"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 每个测试分配独立目录计数器（测试并行运行，目录必须互不干扰）
    static DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// 创建独立测试目录与 Store
    fn test_store() -> (Store, PathBuf) {
        let seq = DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "cliplite-test-{}-{seq}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let store = Store::new(&dir).expect("创建测试 Store 失败");
        (store, dir)
    }

    /// 生成 2x2 红色 PNG 字节
    fn make_png() -> Vec<u8> {
        use image::codecs::png::PngEncoder;
        use image::{ExtendedColorType, ImageEncoder, Rgba, RgbaImage};
        let img = RgbaImage::from_pixel(2, 2, Rgba([255, 0, 0, 255]));
        let mut buf = Vec::new();
        PngEncoder::new(&mut std::io::Cursor::new(&mut buf))
            .write_image(&img.into_raw(), 2, 2, ExtendedColorType::Rgba8)
            .expect("PNG 编码失败");
        buf
    }

    #[test]
    fn test_dedup() {
        let (store, dir) = test_store();
        assert!(
            store.insert_text("hello", None).unwrap().is_some(),
            "首次插入应新增"
        );
        assert!(
            store.insert_text("hello", None).unwrap().is_none(),
            "重复插入应去重"
        );
        assert_eq!(store.list(None).unwrap().len(), 1);
        // 去重会刷新 created_at，但条数不变
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.insert_text("hello", None).unwrap();
        assert_eq!(store.list(None).unwrap().len(), 1);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_image_dedup() {
        let (store, dir) = test_store();
        let png = make_png();
        // 首次入库：临时文件被重命名到图片目录
        let f1 = dir.join("a.tmp");
        fs::write(&f1, &png).unwrap();
        let item = store.insert_image(&f1, None).unwrap();
        assert!(item.is_some(), "首次图片入库应新增");
        assert_eq!(item.unwrap().kind, "image");
        assert!(!f1.exists(), "入库后临时文件应被重命名");

        // 重复图片：去重且删除临时文件
        let f2 = dir.join("b.tmp");
        fs::write(&f2, &png).unwrap();
        assert!(store.insert_image(&f2, None).unwrap().is_none(), "重复图片应去重");
        assert!(!f2.exists(), "重复图片的临时文件应被删除");

        let items = store.list(None).unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].image_path.as_deref().unwrap().ends_with(".png"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_source_recorded() {
        let (store, dir) = test_store();
        let item = store
            .insert_text("来自浏览器的内容", Some(("chrome.exe", "知乎 - Chrome")))
            .unwrap()
            .unwrap();
        assert_eq!(item.source_app.as_deref(), Some("chrome.exe"));
        assert_eq!(item.source_title.as_deref(), Some("知乎 - Chrome"));
        // 来源为空的条目字段为 None
        let item2 = store.insert_text("无来源", None).unwrap().unwrap();
        assert!(item2.source_app.is_none());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_migration_idempotent() {
        // 新库直接打开两次应不报错（migrate_columns 幂等）
        let (store, dir) = test_store();
        drop(store);
        let store2 = Store::new(&dir).expect("重复打开应成功");
        drop(store2);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_snippet_crud_and_groups() {
        let (store, dir) = test_store();
        // 新增与默认分组
        let s1 = store
            .add_snippet("你好，世界", Some("问候"), "工作")
            .unwrap();
        assert_eq!(s1.group_name, "工作");
        let s2 = store.add_snippet("纯内容片段", None, "").unwrap();
        assert_eq!(s2.group_name, "默认", "空分组名应回退「默认」");
        assert_eq!(store.list_snippets().unwrap().len(), 2);

        // 更新内容与分组
        let s1u = store
            .update_snippet(s1.id, Some("新的内容"), None, Some("个人"))
            .unwrap();
        assert_eq!(s1u.content, "新的内容");
        assert_eq!(s1u.group_name, "个人");
        // 标题留空（None）保持原值
        let s1u2 = store.update_snippet(s1.id, None, None, None).unwrap();
        assert_eq!(s1u2.title.as_deref(), Some("问候"));

        // 重命名分组：目标重名报错
        store.add_snippet("归组", None, "个人").unwrap();
        store.add_snippet("留在工作", None, "工作").unwrap();
        assert!(store.rename_group("个人", "工作").is_err(), "重名应报错");
        store.rename_group("个人", "私密").unwrap();
        let groups: Vec<String> = store
            .list_snippets()
            .unwrap()
            .iter()
            .map(|s| s.group_name.clone())
            .collect();
        assert!(!groups.iter().any(|g| g == "个人"));
        assert!(groups.iter().any(|g| g == "私密"));

        // 删除分组：组内片段移入默认
        let moved = store.delete_group("私密").unwrap();
        assert_eq!(moved, 2, "私密组应有 2 条被迁移");
        let all = store.list_snippets().unwrap();
        assert!(
            all.iter().all(|s| s.group_name != "私密"),
            "删除后不应再出现该分组"
        );
        assert_eq!(
            all.iter()
                .filter(|s| s.content == "新的内容" || s.content == "归组")
                .filter(|s| s.group_name == "默认")
                .count(),
            2,
            "原私密组的片段应移入默认组"
        );
        // 默认组不可删
        assert!(store.delete_group("默认").is_err());

        // 删除片段
        store.delete_snippet(s2.id).unwrap();
        assert_eq!(store.list_snippets().unwrap().len(), 3);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_group_independent_persistence() {
        let (store, dir) = test_store();
        // 新建空分组（无片段）：应独立持久化并出现在分组列表
        store.add_group("空分组").unwrap();
        let groups = store.list_groups().unwrap();
        assert!(groups.contains(&"空分组".to_string()), "空分组应出现在分组列表");
        assert!(groups.contains(&"默认".to_string()), "默认组应始终存在");
        assert!(store.add_group("空分组").is_err(), "重名分组应报错");

        // 重新打开数据库：空分组仍保留
        drop(store);
        let store2 = Store::new(&dir).unwrap();
        assert!(
            store2.list_groups().unwrap().contains(&"空分组".to_string()),
            "重启后空分组应保留"
        );

        // 向空分组添加片段 → 片段进入该组
        let s = store2.add_snippet("内容", None, "空分组").unwrap();
        assert_eq!(s.group_name, "空分组");

        // 重命名分组 → 片段与分组表同步更新
        store2.rename_group("空分组", "新名").unwrap();
        let groups2 = store2.list_groups().unwrap();
        assert!(!groups2.contains(&"空分组".to_string()));
        assert!(groups2.contains(&"新名".to_string()));
        assert_eq!(store2.list_snippets().unwrap()[0].group_name, "新名");

        // 删除分组 → 分组表记录删除，片段移入默认
        store2.delete_group("新名").unwrap();
        assert!(!store2.list_groups().unwrap().contains(&"新名".to_string()));
        assert_eq!(store2.list_snippets().unwrap()[0].group_name, "默认");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_capacity_trim_keeps_pinned() {
        let (store, dir) = test_store();
        let limit = crate::settings::DEFAULT_MAX_ITEMS;
        // 先插一条并置顶
        let pinned = store.insert_text("pinned-item", None).unwrap().unwrap();
        store.toggle_pin(pinned.id).unwrap();
        // 再插入超过上限的条目
        for i in 0..(limit + 10) {
            store.insert_text(&format!("bulk-{i}"), None).unwrap();
        }
        let items = store.list(None).unwrap();
        assert!(items.len() as i64 <= limit, "超限后应被清理到上限以内");
        assert_eq!(items[0].id, pinned.id, "置顶条目应保留且排在最前");
        assert_eq!(items.iter().filter(|i| i.pinned).count(), 1, "置顶条目不应被清理");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_capacity_trim_custom_limit() {
        let (store, dir) = test_store();
        let mut store = store;
        // set_max_items 有下限保护（50），测试值须高于下限
        store.set_max_items(60);
        for i in 0..80 {
            store.insert_text(&format!("bulk-{i}"), None).unwrap();
        }
        assert_eq!(store.list(None).unwrap().len(), 60, "自定义上限应生效");
        // 调低后旧数据也立即清理到新上限内（直查库验证，非仅列表截断）
        store.set_max_items(55);
        let total: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 55, "降低上限应立即清理");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_pinned_sort() {
        let (store, dir) = test_store();
        let a = store.insert_text("a", None).unwrap().unwrap();
        let b = store.insert_text("b", None).unwrap().unwrap();
        let c = store.insert_text("c", None).unwrap().unwrap();
        store.toggle_pin(b.id).unwrap();
        let items = store.list(None).unwrap();
        let ids: Vec<i64> = items.iter().map(|i| i.id).collect();
        assert_eq!(ids, vec![b.id, c.id, a.id], "置顶优先，其余按时间倒序");
        // 取消置顶后回到纯时间倒序（toggle 不改变 created_at，b 仍位于中间）
        store.toggle_pin(b.id).unwrap();
        let items = store.list(None).unwrap();
        let ids: Vec<i64> = items.iter().map(|i| i.id).collect();
        assert_eq!(ids, vec![c.id, b.id, a.id]);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_search() {
        let (store, dir) = test_store();
        store.insert_text("hello world", None).unwrap();
        store.insert_text("rust rocks", None).unwrap();
        store.insert_text("hello rust", None).unwrap();
        let items = store.list(Some("rust")).unwrap();
        assert_eq!(items.len(), 2);
        assert!(
            items.iter().all(|i| i.content.as_deref().unwrap().contains("rust")),
            "搜索结果都应包含关键字"
        );
        assert_eq!(store.list(Some("  ")).unwrap().len(), 3, "空白关键字视为全量查询");
        assert!(store.list(Some("不存在")).unwrap().is_empty());
        // 通配符应被转义而非作为模式
        assert!(store.list(Some("%")).unwrap().is_empty());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_delete_and_clear() {
        let (store, dir) = test_store();
        store.insert_text("t1", None).unwrap();
        let png = make_png();
        let f = dir.join("c.tmp");
        fs::write(&f, &png).unwrap();
        let img = store.insert_image(&f, None).unwrap().unwrap();
        let img_file = img.image_path.clone().unwrap();

        // 删除文本条目
        let items = store.list(None).unwrap();
        let text_id = items.iter().find(|i| i.kind == "text").unwrap().id;
        store.delete(text_id).unwrap();
        assert_eq!(store.list(None).unwrap().len(), 1);

        // 删除图片条目会连带删除图片文件
        store.delete(img.id).unwrap();
        assert!(!Path::new(&img_file).exists(), "删除图片条目应同时删除文件");

        // 清空
        let f2 = dir.join("d.tmp");
        fs::write(&f2, &png).unwrap();
        store.insert_image(&f2, None).unwrap();
        store.clear().unwrap();
        assert!(store.list(None).unwrap().is_empty());
        let mut entries = fs::read_dir(store.images_dir()).unwrap();
        assert!(entries.next().is_none(), "清空后图片目录应无残留文件");
        fs::remove_dir_all(&dir).ok();
    }
}
