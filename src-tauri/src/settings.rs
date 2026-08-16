//! 应用设置：hotkey / clear_on_exit / autostart，持久化到 app_config_dir()/settings.json。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut};

/// 默认呼出热键
pub const DEFAULT_HOTKEY: &str = "Ctrl+Shift+V";

/// 应用设置
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    /// 全局呼出热键，如 "Ctrl+Shift+V"
    pub hotkey: String,
    /// 退出时是否清空历史（兼容 camelCase 写法：clearOnExit）
    #[serde(alias = "clearOnExit")]
    pub clear_on_exit: bool,
    /// 是否开机自启
    pub autostart: bool,
    /// 毛玻璃染色层不透明度（0.30~0.62，越小透出越强）
    #[serde(alias = "glassAlpha", default = "default_glass_alpha")]
    pub glass_alpha: f64,
}

/// 默认染色不透明度（当前视觉默认值）
fn default_glass_alpha() -> f64 {
    0.46
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: DEFAULT_HOTKEY.to_string(),
            clear_on_exit: false,
            autostart: false,
            glass_alpha: default_glass_alpha(),
        }
    }
}

/// 托管在 Tauri 状态中的设置（含配置文件目录，供 update_settings 落盘使用）
pub struct SettingsState {
    pub config_dir: PathBuf,
    pub settings: Mutex<Settings>,
}

/// 读取设置；文件不存在或解析失败时回退默认值并写回默认文件。
pub fn load_settings(config_dir: &Path) -> Settings {
    let path = config_dir.join("settings.json");
    match fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
    {
        Some(s) if !s.hotkey.trim().is_empty() => s,
        _ => {
            let def = Settings::default();
            let _ = save_settings(config_dir, &def);
            def
        }
    }
}

/// 保存设置到配置文件
pub fn save_settings(config_dir: &Path, settings: &Settings) -> Result<(), String> {
    fs::create_dir_all(config_dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    fs::write(config_dir.join("settings.json"), json).map_err(|e| e.to_string())
}

/// 解析用户热键字符串（如 "Ctrl+Shift+V"）为插件的 Shortcut。
/// 支持修饰键：Ctrl/Control、Shift、Alt/Option、Win/Cmd/Command/Super/Meta。
pub fn parse_hotkey(s: &str) -> Result<Shortcut, String> {
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    let Some(key) = parts.last().filter(|k| !k.is_empty()) else {
        return Err("热键格式无效".into());
    };
    let mut modifiers = Modifiers::empty();
    for m in &parts[..parts.len() - 1] {
        match m.to_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= Modifiers::CONTROL,
            "shift" => modifiers |= Modifiers::SHIFT,
            "alt" | "option" => modifiers |= Modifiers::ALT,
            "win" | "cmd" | "command" | "super" | "meta" => modifiers |= Modifiers::SUPER,
            other => return Err(format!("不支持的修饰键: {other}")),
        }
    }
    if modifiers.is_empty() {
        return Err("热键至少需要一个修饰键（如 Ctrl/Shift/Alt/Win）".into());
    }
    let code = parse_code(key)?;
    Ok(Shortcut::new(Some(modifiers), code))
}

/// 把按键名称解析为 keyboard_types 的 Code。
/// 优先尝试标准枚举名（KeyV / Digit1 / F5 / ArrowUp），再匹配友好名称（V / 1 / F5 / Up）。
fn parse_code(key: &str) -> Result<Code, String> {
    let key = key.trim();
    if let Ok(code) = key.parse::<Code>() {
        return Ok(code);
    }
    match key.to_ascii_uppercase().as_str() {
        // 字母键
        "A" => Ok(Code::KeyA),
        "B" => Ok(Code::KeyB),
        "C" => Ok(Code::KeyC),
        "D" => Ok(Code::KeyD),
        "E" => Ok(Code::KeyE),
        "F" => Ok(Code::KeyF),
        "G" => Ok(Code::KeyG),
        "H" => Ok(Code::KeyH),
        "I" => Ok(Code::KeyI),
        "J" => Ok(Code::KeyJ),
        "K" => Ok(Code::KeyK),
        "L" => Ok(Code::KeyL),
        "M" => Ok(Code::KeyM),
        "N" => Ok(Code::KeyN),
        "O" => Ok(Code::KeyO),
        "P" => Ok(Code::KeyP),
        "Q" => Ok(Code::KeyQ),
        "R" => Ok(Code::KeyR),
        "S" => Ok(Code::KeyS),
        "T" => Ok(Code::KeyT),
        "U" => Ok(Code::KeyU),
        "V" => Ok(Code::KeyV),
        "W" => Ok(Code::KeyW),
        "X" => Ok(Code::KeyX),
        "Y" => Ok(Code::KeyY),
        "Z" => Ok(Code::KeyZ),
        // 数字键
        "0" => Ok(Code::Digit0),
        "1" => Ok(Code::Digit1),
        "2" => Ok(Code::Digit2),
        "3" => Ok(Code::Digit3),
        "4" => Ok(Code::Digit4),
        "5" => Ok(Code::Digit5),
        "6" => Ok(Code::Digit6),
        "7" => Ok(Code::Digit7),
        "8" => Ok(Code::Digit8),
        "9" => Ok(Code::Digit9),
        // 功能键
        "F1" => Ok(Code::F1),
        "F2" => Ok(Code::F2),
        "F3" => Ok(Code::F3),
        "F4" => Ok(Code::F4),
        "F5" => Ok(Code::F5),
        "F6" => Ok(Code::F6),
        "F7" => Ok(Code::F7),
        "F8" => Ok(Code::F8),
        "F9" => Ok(Code::F9),
        "F10" => Ok(Code::F10),
        "F11" => Ok(Code::F11),
        "F12" => Ok(Code::F12),
        "F13" => Ok(Code::F13),
        "F14" => Ok(Code::F14),
        "F15" => Ok(Code::F15),
        "F16" => Ok(Code::F16),
        "F17" => Ok(Code::F17),
        "F18" => Ok(Code::F18),
        "F19" => Ok(Code::F19),
        "F20" => Ok(Code::F20),
        "F21" => Ok(Code::F21),
        "F22" => Ok(Code::F22),
        "F23" => Ok(Code::F23),
        "F24" => Ok(Code::F24),
        // 常用功能键
        "SPACE" => Ok(Code::Space),
        "ENTER" | "RETURN" => Ok(Code::Enter),
        "TAB" => Ok(Code::Tab),
        "ESC" | "ESCAPE" => Ok(Code::Escape),
        "BACKSPACE" | "BACK" => Ok(Code::Backspace),
        "DELETE" | "DEL" => Ok(Code::Delete),
        "INSERT" | "INS" => Ok(Code::Insert),
        "HOME" => Ok(Code::Home),
        "END" => Ok(Code::End),
        "PAGEUP" | "PGUP" => Ok(Code::PageUp),
        "PAGEDOWN" | "PGDN" => Ok(Code::PageDown),
        "UP" | "ARROWUP" => Ok(Code::ArrowUp),
        "DOWN" | "ARROWDOWN" => Ok(Code::ArrowDown),
        "LEFT" | "ARROWLEFT" => Ok(Code::ArrowLeft),
        "RIGHT" | "ARROWRIGHT" => Ok(Code::ArrowRight),
        // 标点键
        "-" | "MINUS" => Ok(Code::Minus),
        "=" | "EQUAL" | "PLUS" => Ok(Code::Equal),
        "[" | "BRACKETLEFT" => Ok(Code::BracketLeft),
        "]" | "BRACKETRIGHT" => Ok(Code::BracketRight),
        "\\" | "BACKSLASH" => Ok(Code::Backslash),
        ";" | "SEMICOLON" => Ok(Code::Semicolon),
        "'" | "QUOTE" => Ok(Code::Quote),
        "`" | "BACKQUOTE" => Ok(Code::Backquote),
        "," | "COMMA" => Ok(Code::Comma),
        "." | "PERIOD" | "DOT" => Ok(Code::Period),
        "/" | "SLASH" => Ok(Code::Slash),
        _ => Err(format!("不支持的按键: {key}")),
    }
}
