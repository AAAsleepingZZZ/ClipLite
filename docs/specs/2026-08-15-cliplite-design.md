# ClipLite — 轻量美观剪贴板设计文档

日期：2026-08-15 · 状态：已批准实施 · v0.3.0 已扩展

## 需求

Windows 剪贴板管理器：托盘常驻 + 全局热键呼出深色毛玻璃面板，单击复制、双击粘贴。
核心版功能：文本/图片/文件历史、模糊搜索、置顶收藏、全局热键、托盘、SQLite 持久化（500 条自动清理）。
v0.3.0 扩展：来源记录与筛选、常用片段库（分组管理）、容量参数可调、图片文件缩略图。

## 技术选型

- Tauri 2（Rust 1.97 + WebView2），前端原生 HTML/CSS/JS 无构建链
- 剪贴板监听：Win32 `AddClipboardFormatListener` 消息监听（事件驱动零轮询）
- 读取：`arboard`；图片转 PNG 存 `app_data_dir()/images/`（>10MB 跳过）
- 存储：`rusqlite`（bundled），单表 + 内容哈希去重 + 超限清理
- 粘贴：写剪贴板 → 80ms → `SendInput` Ctrl+V
- 毛玻璃：`window-vibrancy`（Mica → Acrylic → Blur 逐级降级）
- 热键：`tauri-plugin-global-shortcut`，默认 `Ctrl+Shift+V`，可重录
- 其他：`tauri-plugin-autostart`、`tauri-plugin-single-instance`

## 架构

Rust 后端（监听/存储/托盘/热键/粘贴）⟷ Tauri IPC ⟷ Web 前端（搜索/列表/置顶/设置）。

窗口：440×640、无边框、透明、置顶、跳过任务栏、启动隐藏；热键呼出时按光标位置定位（右下方 +8px，超屏翻转）；失焦自动隐藏；Esc 关闭。

## IPC 契约

- `get_items({query?})` → `[{id, kind:'text'|'image', content, imagePath, pinned, createdAt}]`
- `copy_item({id})` / `paste_item({id})`（复制并模拟粘贴）/ `toggle_pin({id})` / `delete_item({id})` / `clear_history()`
- `get_image({id})` → base64 data URL
- `get_settings()` → `{hotkey, clearOnExit, autostart}`；`update_settings({settings})`（热键失败返回错误）
- `hide_panel()`
- 事件：`clipboard-updated`（新记录入库后 emit，前端刷新）

## 数据模型

```sql
CREATE TABLE items (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  kind TEXT NOT NULL, content TEXT, image_path TEXT,
  hash TEXT NOT NULL UNIQUE, pinned INTEGER DEFAULT 0,
  created_at INTEGER NOT NULL,
  source_app TEXT,        -- v0.3：来源应用 exe 名（旧库自动迁移补列）
  source_title TEXT       -- v0.3：来源窗口标题
);

CREATE TABLE snippets (   -- v0.3：常用片段库（仅文本，分组用 group_name 字段）
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  content TEXT NOT NULL,
  title TEXT,
  group_name TEXT NOT NULL DEFAULT '默认',
  created_at INTEGER NOT NULL
);

CREATE TABLE snippet_groups (  -- v0.3：分组独立持久化（空分组也保留，默认组始终存在）
  name TEXT PRIMARY KEY
);
```

## v0.3 IPC 扩展

- `get_snippets()` → `[{id, content, title, groupName, createdAt}]`
- `add_snippet({content, title?, groupName?})` / `update_snippet({id, content?, title?, groupName?})` / `delete_snippet({id})`
- `get_groups()` → `[groupName...]`（独立分组表，含空分组）
- `add_group({name})`（重名报错）/ `rename_group({oldName, newName})`（重名报错）/ `delete_group({name})`（组内片段移入「默认」）
- `copy_text({content})` / `paste_text({content})`（片段单击复制、双击/回车粘贴）
- `get_file_thumb({id})` → 128x128 方形缩略图 base64（文件条目是图片时预览）
- `get_items` 条目新增 `sourceApp` / `sourceTitle` 字段
- 前端片段条目与分组 chip 均有 ⋮ 按钮：打开与右键一致的管理菜单

## v0.3 设置扩展

- `max_items`（历史条数上限，默认 500，范围 50~5000，调低立即清理）
- `max_image_mb`（图片大小上限 MB，默认 10，范围 1~100）

## 边界情况（v0.3 补充）

- 来源获取失败静默跳过（前台窗口句柄失效/权限不足时来源为空，不阻塞入库）
- 来源为 ClipLite 自身窗口时不记录（面板打开时复制的内容仍正常入库，只是无来源）
- 图片文件缩略图仅对常见位图扩展名（png/jpg/jpeg/gif/webp/bmp）生成，其余保持文件图标

## 边界情况

- 防回环：自身写入后 200ms 内的监听事件忽略
- 去重：同 hash 更新 created_at 置顶，不重复插入
- 剪贴板被占用/读取失败：跳过本次不崩溃
- 双击粘贴：贴到当前前台窗口；超 500 条清理最旧非置顶条目

## 验收

cargo test 通过；手动清单：复制文本/图片→热键呼出→搜索→置顶→双击粘贴→重启持久→超限清理→改热键→自启动→打包安装。
