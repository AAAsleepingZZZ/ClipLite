# ClipLite — 轻量美观剪贴板设计文档

日期：2026-08-15 · 状态：已批准实施

## 需求

Windows 剪贴板管理器：托盘常驻 + 全局热键呼出深色毛玻璃面板，单击复制、双击粘贴。
核心版功能：文本/图片历史、模糊搜索、置顶收藏、全局热键、托盘、SQLite 持久化（500 条自动清理）。

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
  created_at INTEGER NOT NULL
);
```

## 边界情况

- 防回环：自身写入后 200ms 内的监听事件忽略
- 去重：同 hash 更新 created_at 置顶，不重复插入
- 剪贴板被占用/读取失败：跳过本次不崩溃
- 双击粘贴：贴到当前前台窗口；超 500 条清理最旧非置顶条目

## 验收

cargo test 通过；手动清单：复制文本/图片→热键呼出→搜索→置顶→双击粘贴→重启持久→超限清理→改热键→自启动→打包安装。
