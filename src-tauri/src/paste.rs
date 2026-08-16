//! 模拟 Ctrl+V 粘贴：先等待剪贴板就绪，再通过 SendInput 发送按键序列。

use std::mem::size_of;
use std::thread;
use std::time::Duration;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VK_CONTROL,
    VK_V, VIRTUAL_KEY,
};

/// 写入剪贴板后等待系统/目标程序就绪的时间（毫秒）
const PASTE_DELAY_MS: u64 = 80;

/// 向当前焦点窗口发送 Ctrl+V 粘贴。
pub fn paste() -> Result<(), String> {
    thread::sleep(Duration::from_millis(PASTE_DELAY_MS));

    // 依次发送：Ctrl 按下 → V 按下 → V 抬起 → Ctrl 抬起
    let inputs = [
        keyboard_input(VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
        keyboard_input(VK_V, KEYBD_EVENT_FLAGS(0)),
        keyboard_input(VK_V, KEYEVENTF_KEYUP),
        keyboard_input(VK_CONTROL, KEYEVENTF_KEYUP),
    ];

    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    if sent == inputs.len() as u32 {
        Ok(())
    } else {
        Err(format!(
            "SendInput 仅成功发送 {sent}/{} 个输入事件",
            inputs.len()
        ))
    }
}

/// 构造一个键盘输入事件
fn keyboard_input(vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    let mut input: INPUT = unsafe { std::mem::zeroed() };
    input.r#type = INPUT_KEYBOARD;
    input.Anonymous.ki = KEYBDINPUT {
        wVk: vk,
        wScan: 0,
        dwFlags: flags,
        time: 0,
        dwExtraInfo: 0,
    };
    input
}
