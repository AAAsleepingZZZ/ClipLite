//! 剪贴板历史存储：基于 rusqlite（SQLite 内嵌）的单表存储。
//!
//! 表结构：
//! items(id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT, content TEXT NULL,
//!       image_path TEXT NULL, hash TEXT NOT NULL UNIQUE,
//!       pinned INTEGER DEFAULT 0, created_at INTEGER)

use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::clipboard::now_ms;

/// 历史记录上限，超出后清理最旧的非置顶条目
const MAX_ITEMS: i64 = 500;

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
}

/// 存储层统一错误类型
pub type StoreError = Box<dyn Error + Send + Sync>;

/// 剪贴板历史存储
pub struct Store {
    conn: Connection,
    data_dir: PathBuf,
}

impl Store {
    /// 打开（或创建）数据目录下的 SQLite 数据库，并确保表结构与图片目录存在。
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
        Ok(Self {
            conn,
            data_dir: data_dir.to_path_buf(),
        })
    }

    /// 图片文件存放目录
    pub fn images_dir(&self) -> PathBuf {
        self.data_dir.join("images")
    }

    /// 插入一条文本记录。
    /// 内容已存在时（按 hash 查重）仅刷新 created_at 并返回 `Ok(None)` 表示未新增；
    /// 否则插入并返回新条目。
    pub fn insert_text(&self, content: &str) -> Result<Option<Item>, StoreError> {
        if content.trim().is_empty() {
            return Ok(None);
        }
        let hash = hash_bytes(content.as_bytes());
        let now = now_ms() as i64;
        if self.touch_existing(&hash, now)? {
            return Ok(None);
        }
        self.conn.execute(
            "INSERT INTO items (kind, content, image_path, hash, pinned, created_at)
             VALUES ('text', ?1, NULL, ?2, 0, ?3)",
            params![content, hash, now],
        )?;
        let id = self.conn.last_insert_rowid();
        self.trim()?;
        Ok(Some(self.get_item(id)?))
    }

    /// 插入一条图片记录。`file_path` 为待入库的图片文件（调用方写入的临时文件）。
    /// 内部按文件内容哈希查重：重复时删除临时文件并返回 `Ok(None)`；
    /// 否则把文件重命名为 `<hash>.png` 放入图片目录，再插入记录。
    pub fn insert_image(&self, file_path: &Path) -> Result<Option<Item>, StoreError> {
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
            "INSERT INTO items (kind, content, image_path, hash, pinned, created_at)
             VALUES ('image', NULL, ?1, ?2, 0, ?3)",
            params![target.to_string_lossy().to_string(), hash, now],
        )?;
        let id = self.conn.last_insert_rowid();
        self.trim()?;
        Ok(Some(self.get_item(id)?))
    }

    /// 插入一条文件记录（复制文件/文件夹时，只记录路径列表，不读内容）。
    /// 路径列表以换行分隔存入 content；按路径列表哈希去重。
    pub fn insert_files(&self, paths: &[PathBuf]) -> Result<Option<Item>, StoreError> {
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
            "INSERT INTO items (kind, content, image_path, hash, pinned, created_at)
             VALUES ('file', ?1, NULL, ?2, 0, ?3)",
            params![content, hash, now],
        )?;
        let id = self.conn.last_insert_rowid();
        self.trim()?;
        Ok(Some(self.get_item(id)?))
    }

    /// 查询历史：`query` 非空时按内容模糊搜索（LIKE，转义通配符），否则返回全部。
    /// 排序：置顶优先，其次按创建时间倒序；最多返回 500 条。
    pub fn list(&self, query: Option<&str>) -> Result<Vec<Item>, StoreError> {
        let q = query.map(str::trim).filter(|s| !s.is_empty());
        let mut items = Vec::new();
        let sql = match q {
            Some(_) => {
                "SELECT id, kind, content, image_path, pinned, created_at FROM items
                 WHERE content LIKE ?1 ESCAPE '\\'
                 ORDER BY pinned DESC, created_at DESC LIMIT 500"
            }
            None => {
                "SELECT id, kind, content, image_path, pinned, created_at FROM items
                 ORDER BY pinned DESC, created_at DESC LIMIT 500"
            }
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = match q {
            Some(q) => stmt.query_map(params![format!("%{}%", escape_like(q))], row_to_item)?,
            None => stmt.query_map([], row_to_item)?,
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
                "SELECT id, kind, content, image_path, pinned, created_at FROM items WHERE id = ?1",
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
        if total <= MAX_ITEMS {
            return Ok(());
        }
        let excess = total - MAX_ITEMS;
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
    })
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
        assert!(store.insert_text("hello").unwrap().is_some(), "首次插入应新增");
        assert!(store.insert_text("hello").unwrap().is_none(), "重复插入应去重");
        assert_eq!(store.list(None).unwrap().len(), 1);
        // 去重会刷新 created_at，但条数不变
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.insert_text("hello").unwrap();
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
        let item = store.insert_image(&f1).unwrap();
        assert!(item.is_some(), "首次图片入库应新增");
        assert_eq!(item.unwrap().kind, "image");
        assert!(!f1.exists(), "入库后临时文件应被重命名");

        // 重复图片：去重且删除临时文件
        let f2 = dir.join("b.tmp");
        fs::write(&f2, &png).unwrap();
        assert!(store.insert_image(&f2).unwrap().is_none(), "重复图片应去重");
        assert!(!f2.exists(), "重复图片的临时文件应被删除");

        let items = store.list(None).unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].image_path.as_deref().unwrap().ends_with(".png"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_capacity_trim_keeps_pinned() {
        let (store, dir) = test_store();
        // 先插一条并置顶
        let pinned = store.insert_text("pinned-item").unwrap().unwrap();
        store.toggle_pin(pinned.id).unwrap();
        // 再插入超过上限的条目
        for i in 0..(MAX_ITEMS + 10) {
            store.insert_text(&format!("bulk-{i}")).unwrap();
        }
        let items = store.list(None).unwrap();
        assert!(items.len() as i64 <= MAX_ITEMS, "超限后应被清理到上限以内");
        assert_eq!(items[0].id, pinned.id, "置顶条目应保留且排在最前");
        assert_eq!(items.iter().filter(|i| i.pinned).count(), 1, "置顶条目不应被清理");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_pinned_sort() {
        let (store, dir) = test_store();
        let a = store.insert_text("a").unwrap().unwrap();
        let b = store.insert_text("b").unwrap().unwrap();
        let c = store.insert_text("c").unwrap().unwrap();
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
        store.insert_text("hello world").unwrap();
        store.insert_text("rust rocks").unwrap();
        store.insert_text("hello rust").unwrap();
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
        store.insert_text("t1").unwrap();
        let png = make_png();
        let f = dir.join("c.tmp");
        fs::write(&f, &png).unwrap();
        let img = store.insert_image(&f).unwrap().unwrap();
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
        store.insert_image(&f2).unwrap();
        store.clear().unwrap();
        assert!(store.list(None).unwrap().is_empty());
        let mut entries = fs::read_dir(store.images_dir()).unwrap();
        assert!(entries.next().is_none(), "清空后图片目录应无残留文件");
        fs::remove_dir_all(&dir).ok();
    }
}
