//! The app's colors per theme. The window reads them while it is built; a theme change builds it
//! again (`rebuild`), so nothing holds an old color.

use std::cell::Cell;

/// Every color the window uses, as 0xRRGGBB (`scroll_track` is 0xAARRGGBB).
pub struct Palette {
    /// The window behind everything.
    pub bg: u32,
    /// A resting session row, and the field of the search box.
    pub row: u32,
    /// A row that needs a look (waiting, working, just finished): a little brighter than `row`.
    pub row_active: u32,
    /// Menus and popups.
    pub panel: u32,
    /// Borders, dividers, a menu item under the mouse, a switch's track when off.
    pub border: u32,
    /// The background of the banners (reopen, update).
    pub banner: u32,
    /// Names, titles, answers.
    pub text: u32,
    /// Ordinary text (menu items, the confirm question).
    pub text_body: u32,
    /// Second lines, notes, the counts.
    pub text_soft: u32,
    /// Quiet text: times, the version, placeholders.
    pub muted: u32,
    /// A switch's thumb when off.
    pub thumb_off: u32,
    pub accent: u32,
    pub danger: u32,
    // Status dots and the status text on the right of a row.
    pub waiting: u32,
    pub busy: u32,
    pub finished: u32,
    pub question: u32,
    pub idle: u32,
    pub scroll_thumb: u32,
    pub scroll_track: u32,
    /// What a row, button or switch is mixed with under the mouse, and how much.
    pub hover_toward: u32,
    pub hover_row: f32,
    pub hover_control: f32,
}

pub const DARK: Palette = Palette {
    bg: 0x12141C,
    row: 0x1C2030,
    row_active: 0x30374B,
    panel: 0x262B3B,
    border: 0x3B4256,
    banner: 0x1E2A44,
    text: 0xFFFFFF,
    text_body: 0xE5E7EB,
    text_soft: 0x9CA3AF,
    muted: 0x6B7280,
    thumb_off: 0xC9CDD4,
    accent: 0x3B82F6,
    danger: 0xDC2626,
    waiting: 0xFACC15,
    busy: 0x22C55E,
    finished: 0xEF4444,
    question: 0x3B9EDB,
    idle: 0x6B7280,
    scroll_thumb: 0x4B5563,
    scroll_track: 0x14FF_FFFF,
    hover_toward: 0xFFFFFF,
    hover_row: 0.06,
    hover_control: 0.15,
};

/// The same roles on white: status colors a shade deeper, so their text reads on a light row.
pub const LIGHT: Palette = Palette {
    bg: 0xF3F4F6,
    row: 0xFFFFFF,
    row_active: 0xE4E9F2,
    panel: 0xFFFFFF,
    border: 0xD1D5DB,
    banner: 0xE8F0FE,
    text: 0x111827,
    text_body: 0x1F2937,
    text_soft: 0x4B5563,
    muted: 0x6B7280,
    thumb_off: 0xFFFFFF,
    accent: 0x2563EB,
    danger: 0xDC2626,
    waiting: 0xCA8A04,
    busy: 0x16A34A,
    finished: 0xDC2626,
    question: 0x2380C2,
    idle: 0x9CA3AF,
    scroll_thumb: 0xB0B7C3,
    scroll_track: 0x14000000,
    hover_toward: 0x000000,
    hover_row: 0.04,
    hover_control: 0.10,
};

thread_local! {
    static CURRENT: Cell<&'static Palette> = const { Cell::new(&DARK) };
}

/// The colors the window is built with now.
pub fn pal() -> &'static Palette {
    CURRENT.with(Cell::get)
}

/// Uses `choice` ("system", "dark", "light") for the next build.
pub fn set(choice: &str) {
    let dark = match choice {
        "light" => false,
        "dark" => true,
        _ => crate::platform::system_dark(),
    };
    CURRENT.with(|c| c.set(if dark { &DARK } else { &LIGHT }));
}
