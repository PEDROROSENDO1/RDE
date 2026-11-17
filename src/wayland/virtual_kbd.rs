use std::io::Write;
use std::os::fd::AsFd;
use std::time::{SystemTime, UNIX_EPOCH};
use wayland_client::{Connection, Dispatch, QueueHandle, globals::GlobalList, protocol::wl_seat};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};

#[derive(Debug, Clone, Copy)]
pub struct ShortcutChord {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub evdev_key: u32,
}

impl ShortcutChord {
    pub const fn ctrl(evdev_key: u32) -> Self {
        Self {
            ctrl: true,
            shift: false,
            alt: false,
            evdev_key,
        }
    }

    pub const fn ctrl_shift(evdev_key: u32) -> Self {
        Self {
            ctrl: true,
            shift: true,
            alt: false,
            evdev_key,
        }
    }

    pub const fn key_only(evdev_key: u32) -> Self {
        Self {
            ctrl: false,
            shift: false,
            alt: false,
            evdev_key,
        }
    }

    pub const fn alt(evdev_key: u32) -> Self {
        Self {
            ctrl: false,
            shift: false,
            alt: true,
            evdev_key,
        }
    }

    pub const fn ctrl_alt(evdev_key: u32) -> Self {
        Self {
            ctrl: true,
            shift: false,
            alt: true,
            evdev_key,
        }
    }
}

// Linux evdev keycodes (as expected by zwp_virtual_keyboard_v1 when using xkb_v1 keymap)
pub const KEY_ESC: u32 = 1;
pub const KEY_1: u32 = 2;
pub const KEY_2: u32 = 3;
pub const KEY_3: u32 = 4;
pub const KEY_MINUS: u32 = 12;
pub const KEY_EQUAL: u32 = 13;
pub const KEY_0: u32 = 11;
pub const KEY_TAB: u32 = 15;
pub const KEY_Q: u32 = 16;
pub const KEY_W: u32 = 17;
pub const KEY_E: u32 = 18;
pub const KEY_R: u32 = 19;
pub const KEY_T: u32 = 20;
pub const KEY_Y: u32 = 21;
pub const KEY_U: u32 = 22;
pub const KEY_I: u32 = 23;
pub const KEY_O: u32 = 24;
pub const KEY_P: u32 = 25;
pub const KEY_A: u32 = 30;
pub const KEY_S: u32 = 31;
pub const KEY_D: u32 = 32;
pub const KEY_F: u32 = 33;
pub const KEY_G: u32 = 34;
pub const KEY_H: u32 = 35;
pub const KEY_J: u32 = 36;
pub const KEY_K: u32 = 37;
pub const KEY_L: u32 = 38;
pub const KEY_Z: u32 = 44;
pub const KEY_X: u32 = 45;
pub const KEY_C: u32 = 46;
pub const KEY_V: u32 = 47;
pub const KEY_B: u32 = 48;
pub const KEY_N: u32 = 49;
pub const KEY_M: u32 = 50;
pub const KEY_COMMA: u32 = 51;
pub const KEY_DOT: u32 = 52;
pub const KEY_SLASH: u32 = 53;
pub const KEY_F1: u32 = 59;
pub const KEY_F3: u32 = 61;
pub const KEY_F5: u32 = 63;
pub const KEY_F9: u32 = 67;
pub const KEY_F11: u32 = 87;
pub const KEY_F12: u32 = 88;
pub const KEY_INSERT: u32 = 110;
pub const KEY_DELETE: u32 = 111;

pub struct VirtualKeyboardController {
    manager: Option<ZwpVirtualKeyboardManagerV1>,
    vkbd: Option<ZwpVirtualKeyboardV1>,
    keymap_uploaded: bool,
}

impl VirtualKeyboardController {
    pub fn bind<State>(globals: &GlobalList, qh: &QueueHandle<State>) -> Self
    where
        State: Dispatch<ZwpVirtualKeyboardManagerV1, ()> + 'static,
    {
        let manager = globals
            .bind::<ZwpVirtualKeyboardManagerV1, State, ()>(qh, 1..=1, ())
            .ok();
        Self {
            manager,
            vkbd: None,
            keymap_uploaded: false,
        }
    }

    pub fn ensure_device<State>(&mut self, seat: &wl_seat::WlSeat, qh: &QueueHandle<State>)
    where
        State: Dispatch<ZwpVirtualKeyboardV1, ()> + 'static,
    {
        if self.vkbd.is_none() {
            if let Some(ref mgr) = self.manager {
                let vkbd = mgr.create_virtual_keyboard(seat, qh, ());
                self.vkbd = Some(vkbd);
                self.keymap_uploaded = false;
            }
        }
        if !self.keymap_uploaded {
            if let Some(ref vkbd) = self.vkbd {
                if upload_default_xkb_keymap(vkbd).is_ok() {
                    self.keymap_uploaded = true;
                }
            }
        }
    }

    /// Sends a real keyboard shortcut chord (e.g. Ctrl+C, Ctrl+S, Ctrl+Z, F11) to the currently focused Wayland window!
    pub fn send_chord(&self, conn: &Connection, chord: ShortcutChord) {
        let Some(ref vkbd) = self.vkbd else {
            return;
        };
        if !self.keymap_uploaded {
            return;
        }

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u32)
            .unwrap_or(100);

        // XKB modifier bitmask in standard us(pc105) keymap:
        // Shift = 1 << 0 (1), Lock = 1 << 1 (2), Control = 1 << 2 (4), Mod1/Alt = 1 << 3 (8)
        let mut mods_depressed = 0u32;
        if chord.shift {
            mods_depressed |= 1;
        }
        if chord.ctrl {
            mods_depressed |= 4;
        }
        if chord.alt {
            mods_depressed |= 8;
        }

        if mods_depressed != 0 {
            vkbd.modifiers(mods_depressed, 0, 0, 0);
        }

        // Press key (WL_KEYBOARD_KEY_STATE_PRESSED = 1)
        vkbd.key(now_ms, chord.evdev_key, 1);
        let _ = conn.flush();

        // Release key (WL_KEYBOARD_KEY_STATE_RELEASED = 0)
        vkbd.key(now_ms.wrapping_add(12), chord.evdev_key, 0);

        // Clear modifiers
        if mods_depressed != 0 {
            vkbd.modifiers(0, 0, 0, 0);
        }
        let _ = conn.flush();
    }
}

fn upload_default_xkb_keymap(vkbd: &ZwpVirtualKeyboardV1) -> Result<(), std::io::Error> {
    // Generate a real XKB_KEYMAP_FORMAT_TEXT_V1 keymap using xkbcommon or a standard PC105 keymap
    let keymap_str = build_xkb_keymap_string();
    let bytes = keymap_str.as_bytes();

    // Create an anonymous file in RAM (/tmp or memfd) with NUL-terminated keymap text
    let mut file = create_shm_keymap_file()?;
    file.write_all(bytes)?;
    file.write_all(&[0u8])?;
    file.flush()?;

    let total_size = (bytes.len() + 1) as u32;
    // format = 1 (WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1)
    vkbd.keymap(1, file.as_fd(), total_size);
    Ok(())
}

fn create_shm_keymap_file() -> Result<std::fs::File, std::io::Error> {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let path = std::path::PathBuf::from(runtime).join(format!(
        "rocketde-vkbd-keymap-{}.xkb",
        std::process::id()
    ));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)?;
    let _ = std::fs::remove_file(&path);
    Ok(file)
}

fn build_xkb_keymap_string() -> String {
    // Try using xkbcommon crate to compile the exact system keymap first
    let context = xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS);
    if let Some(keymap) = xkbcommon::xkb::Keymap::new_from_names(
        &context,
        "",
        "",
        "us",
        "",
        None,
        xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
    ) {
        return keymap.get_as_string(xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1);
    }

    String::from(
        "xkb_keymap {\n\
         xkb_keycodes  { include \"evdev+aliases(qwerty)\" };\n\
         xkb_types     { include \"complete\" };\n\
         xkb_compat    { include \"complete\" };\n\
         xkb_symbols   { include \"pc+us+inet(evdev)\" };\n\
         xkb_geometry  { include \"pc(pc105)\" };\n\
         };\n",
    )
}
