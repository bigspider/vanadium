use core::ops::Range;

pub mod canvas;
pub mod screen;
/// An accelerated `embedded-graphics` `DrawTarget` (requires the `embedded-graphics` feature).
#[cfg(feature = "embedded-graphics")]
pub mod screen_target;

use alloc::format;
use alloc::vec::Vec;

use common::ecall_constants::DEVICE_PROPERTY_ID;
pub use common::ux::{
    Action, ButtonEvent, Deserializable, Event, EventCode, EventData, Icon, NavInfo,
    NavigationInfo, Page, PageContent, PageContentInfo, TagValue, TouchEvent, TouchState,
};

use crate::ecalls;
use crate::ui::{
    button, draw_icon_centered, icons, nav_from_button, touch_release, wrap_lines, Align, Color,
    Font, InputModel, Nav, Rect, Scene, Surface,
};

// Returns true if the device supports the page UX model, false if it supports the step UX model.
// It panics for unsupported devices
pub fn has_page_api() -> bool {
    match ecalls::get_device_property(DEVICE_PROPERTY_ID) {
        0 => true,           // native target
        0x2c970060 => true,  // Ledger Stax
        0x2c970070 => true,  // Ledger Flex
        0x2c970080 => true,  // Ledger Apex_p
        0x2c970040 => false, // Ledger Nano X
        0x2c970050 => false, // Ledger Nano S+
        _ => panic!("Unsupported device"),
    }
}

/// Blocks until an event is received, then returns it.
pub async fn get_event() -> Event {
    loop {
        let mut event_data = EventData::default();
        // SAFETY: event_data is a properly aligned, initialized EventData on the stack.
        let event_code = EventCode::from(unsafe { ecalls::get_event(&mut event_data) });
        match event_code {
            EventCode::Ticker => {
                // Give a chance to the executor to make progress on registered tasks
                crate::executor::yield_now().await;

                return Event::Ticker;
            }
            EventCode::Action => {
                let action = unsafe { event_data.action };
                return Event::Action(action);
            }
            EventCode::Touch => {
                let touch = unsafe { event_data.touch };
                return Event::Touch(touch);
            }
            EventCode::Button => {
                let button = unsafe { event_data.button };
                return Event::Button(button);
            }
            EventCode::Unknown => {
                let data = unsafe { event_data.raw };
                return Event::Unknown(data);
            }
        }
    }
}

// waits for a number of ticker events
pub async fn wait(n: u32) {
    let mut n_tickers = 0u32;
    loop {
        if let Event::Ticker = get_event().await {
            n_tickers += 1;
            if n_tickers >= n {
                return;
            }
        }
    }
}

// Like get_event, but it ignores any event that is not an Action
pub async fn get_action() -> Action {
    loop {
        if let Event::Action(action) = get_event().await {
            return action;
        }
    }
}

// =============================================================================
// Low-level UI flows.
//
// These render directly with the accelerated `Screen` primitives (fill/text/blit) via the
// `ui` layer — they do NOT use NBGL pages or steps, and they consume *raw* input events
// (`Touch`/`Button`) rather than NBGL's semantic `Action`s. The two input models are driven
// from the device `Capabilities`:
//
//  - Pointer (touch) devices show the whole flow on one screen and hit-test taps;
//  - the two-button Nano devices page through the flow with left/right and confirm with
//    both buttons (see `Nav`).
//
// For two-button input we accept *both* the raw `Button` events (the post-NBGL world) and
// the equivalent semantic `Action`s, via `nav_from_event`. This keeps these flows working
// during the migration while a legacy NBGL home screen may still be active and coalescing
// raw buttons into `Action`s (see the SDK `App` dashboard).
// =============================================================================

const MARGIN: i32 = 16;
const BTN_H: i32 = 48;

// Vertical line height of an OS font on this device, in pixels.
fn line_h(surf: &Surface, font: Font) -> i32 {
    surf.caps().font(font).line_height as i32
}

// Word-wraps `text` in `font` to `w` pixels and appends one text node per line starting at
// `y`, returning the `y` just below the block.
fn text_block(
    surf: &Surface,
    sc: &mut Scene,
    x: i32,
    y: i32,
    w: i32,
    text: &str,
    font: Font,
    align: Align,
) -> i32 {
    let lh = line_h(surf, font);
    let lines = wrap_lines(text, w, |s| surf.measure(font, s).w as i32);
    let mut yy = y;
    for line in lines {
        sc.text(
            Rect::new(x, yy, w, lh),
            line,
            font,
            Color::Black,
            Color::White,
            align,
        );
        yy += lh;
    }
    yy
}

// Total pixel height a wrapped block of `text` in `font` would occupy.
fn block_height(surf: &Surface, w: i32, text: &str, font: Font) -> i32 {
    let n = wrap_lines(text, w, |s| surf.measure(font, s).w as i32).len() as i32;
    n * line_h(surf, font)
}

// Maps an input event to a navigation intent for the two-button flows, accepting both raw
// `Button` events and the equivalent NBGL `Action`s (see the module note above).
pub(crate) fn nav_from_event(e: &Event) -> Option<Nav> {
    match e {
        Event::Button(b) => nav_from_button(*b),
        Event::Action(Action::PreviousPage) => Some(Nav::Prev),
        Event::Action(Action::NextPage) => Some(Nav::Next),
        Event::Action(Action::Confirm) => Some(Nav::Select),
        _ => None,
    }
}

// -----------------------------------------------------------------------------
// show_info
// -----------------------------------------------------------------------------

/// Paints a status icon (on the grayscale touch panels) and a centered message, and returns
/// immediately. Shared by [`show_info`] (which then waits) and `App::show_info` (which keeps
/// the app running and clears the screen on a timeout).
pub(crate) fn paint_info(icon: Icon, text: &str) {
    let mut surf = Surface::new();
    let screen = surf.screen();
    let bg = Color::White;

    let content_w = screen.w - 2 * MARGIN;
    let bitmap = icons::gray4(icon); // None on monochrome / no-art icons
    let icon_h = bitmap.as_ref().map(|b| b.h + 16).unwrap_or(0);
    let text_h = block_height(&surf, content_w, text, Font::Large);
    let total = icon_h + text_h;
    let mut y = (screen.h - total) / 2;
    if y < MARGIN {
        y = MARGIN;
    }

    let mut sc = Scene::new();
    sc.rect(screen, bg);
    if let Some(b) = &bitmap {
        let r = draw_icon_centered(&mut sc, screen, b, y);
        y = r.bottom() + 16;
    }
    text_block(&surf, &mut sc, MARGIN, y, content_w, text, Font::Large, Align::Center);
    surf.paint(&sc);
}

/// Shows a status icon (on the grayscale touch panels) and a centered message for ~2s.
pub async fn show_info(icon: Icon, text: &str) {
    paint_info(icon, text);
    wait(20).await; // ~2 seconds
}

// -----------------------------------------------------------------------------
// show_spinner
// -----------------------------------------------------------------------------

/// Draws a "busy" screen with `text` and returns immediately (the caller keeps working).
pub fn show_spinner(text: &str) {
    let mut surf = Surface::new();
    let screen = surf.screen();
    let bg = Color::White;
    let content_w = screen.w - 2 * MARGIN;

    let lh = line_h(&surf, Font::Large);
    let text_h = block_height(&surf, content_w, text, Font::Large);
    let total = text_h + lh; // text + a "..." line
    let y = ((screen.h - total) / 2).max(MARGIN);

    let mut sc = Scene::new();
    sc.rect(screen, bg);
    let y = text_block(&surf, &mut sc, MARGIN, y, content_w, text, Font::Large, Align::Center);
    sc.text(
        Rect::new(MARGIN, y, content_w, lh),
        "...",
        Font::Large,
        Color::Black,
        bg,
        Align::Center,
    );
    surf.paint(&sc);
}

// -----------------------------------------------------------------------------
// show_confirm_reject
// -----------------------------------------------------------------------------

/// Asks the user to confirm or reject. Returns `true` on confirm, `false` on reject.
pub async fn show_confirm_reject(title: &str, text: &str, confirm: &str, reject: &str) -> bool {
    let mut surf = Surface::new();
    if surf.caps().input == InputModel::Pointer {
        confirm_reject_pointer(&mut surf, title, text, confirm, reject).await
    } else {
        confirm_reject_two_button(&mut surf, title, text, confirm, reject).await
    }
}

async fn confirm_reject_pointer(
    surf: &mut Surface,
    title: &str,
    text: &str,
    confirm: &str,
    reject: &str,
) -> bool {
    let screen = surf.screen();
    let bg = Color::White;
    let content_w = screen.w - 2 * MARGIN;

    // Two buttons side by side along the bottom: reject (left), confirm (right).
    let by = screen.h - BTN_H - MARGIN;
    let bw = (content_w - MARGIN) / 2;
    let reject_btn = Rect::new(MARGIN, by, bw, BTN_H);
    let confirm_btn = Rect::new(screen.w - MARGIN - bw, by, bw, BTN_H);

    let mut sc = Scene::new();
    sc.rect(screen, bg);
    let mut y = MARGIN + 8;
    y = text_block(surf, &mut sc, MARGIN, y, content_w, title, Font::Large, Align::Center);
    y += 8;
    text_block(surf, &mut sc, MARGIN, y, content_w, text, Font::Regular, Align::Center);
    button(&mut sc, reject_btn, reject, Font::Bold);
    button(&mut sc, confirm_btn, confirm, Font::Bold);
    surf.paint(&sc);

    loop {
        let event = get_event().await;
        if let Some(p) = touch_release(&event) {
            if confirm_btn.contains(p) {
                return true;
            }
            if reject_btn.contains(p) {
                return false;
            }
        }
    }
}

async fn confirm_reject_two_button(
    surf: &mut Surface,
    title: &str,
    text: &str,
    confirm: &str,
    reject: &str,
) -> bool {
    // Three steps: the message, then a confirm step, then a reject step.
    let n_steps: usize = 3;
    let mut step = 0usize;
    loop {
        let mut sc = Scene::new();
        sc.rect(surf.screen(), Color::White);
        match step {
            0 => draw_two_button_message(surf, &mut sc, title, text, step, n_steps),
            1 => draw_two_button_choice(surf, &mut sc, confirm, Icon::Confirm, step, n_steps),
            _ => draw_two_button_choice(surf, &mut sc, reject, Icon::Reject, step, n_steps),
        }
        surf.paint(&sc);

        loop {
            let event = get_event().await;
            match nav_from_event(&event) {
                Some(Nav::Prev) if step > 0 => {
                    step -= 1;
                    break;
                }
                Some(Nav::Next) if step + 1 < n_steps => {
                    step += 1;
                    break;
                }
                Some(Nav::Select) => match step {
                    1 => return true,
                    2 => return false,
                    _ => {}
                },
                _ => {}
            }
        }
    }
}

// -----------------------------------------------------------------------------
// review_pairs
// -----------------------------------------------------------------------------

/// Reviews a list of tag/value pairs and asks for a final confirmation. Returns `true` if
/// confirmed, `false` if rejected.
pub async fn review_pairs(
    intro_text: &str,
    intro_subtext: &str,
    pairs: &[TagValue],
    final_text: &str,
    final_button_text: &str,
    long_press: bool,
) -> bool {
    let mut surf = Surface::new();
    if surf.caps().input == InputModel::Pointer {
        review_pairs_pointer(
            &mut surf,
            intro_text,
            intro_subtext,
            pairs,
            final_text,
            final_button_text,
            long_press,
        )
        .await
    } else {
        review_pairs_two_button(&mut surf, intro_text, intro_subtext, pairs, final_button_text).await
    }
}

// Greedily packs pairs into pages by measured, wrapped height. At least one pair per page.
fn paginate_pairs(
    surf: &Surface,
    content_w: i32,
    content_h: i32,
    pairs: &[TagValue],
) -> Vec<Range<usize>> {
    let pad = 12;
    let pair_h = |p: &TagValue| {
        block_height(surf, content_w, &p.tag, Font::Bold)
            + block_height(surf, content_w, &p.value, Font::Regular)
            + pad
    };
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < pairs.len() {
        let mut end = start + 1;
        let mut h = pair_h(&pairs[start]);
        while end < pairs.len() {
            let nh = pair_h(&pairs[end]);
            if h + nh > content_h {
                break;
            }
            h += nh;
            end += 1;
        }
        ranges.push(start..end);
        start = end;
    }
    if ranges.is_empty() {
        ranges.push(0..0);
    }
    ranges
}

#[allow(clippy::too_many_arguments)]
async fn review_pairs_pointer(
    surf: &mut Surface,
    intro_text: &str,
    intro_subtext: &str,
    pairs: &[TagValue],
    final_text: &str,
    final_button_text: &str,
    _long_press: bool,
) -> bool {
    let screen = surf.screen();
    let bg = Color::White;
    let content_w = screen.w - 2 * MARGIN;

    let top_h = BTN_H + 8; // top bar: a Cancel button + page indicator
    let bottom_h = BTN_H + 16; // bottom bar: prev / next / confirm
    let content = Rect::new(
        MARGIN,
        top_h,
        content_w,
        screen.h - top_h - bottom_h - MARGIN,
    );

    let ranges = paginate_pairs(surf, content.w, content.h, pairs);
    let n_pages = 2 + ranges.len(); // intro + pair pages + final
    let last = n_pages - 1;

    // Fixed chrome rectangles, hit-tested on touch release.
    let cancel_btn = Rect::new(MARGIN, 4, 120, BTN_H);
    let by = screen.h - BTN_H - 8;
    let nav_w = 130;
    let prev_btn = Rect::new(MARGIN, by, nav_w, BTN_H);
    let next_btn = Rect::new(screen.w - MARGIN - nav_w, by, nav_w, BTN_H);
    let confirm_btn = Rect::new(MARGIN, by, content_w, BTN_H);

    let mut page = 0usize;
    loop {
        let mut sc = Scene::new();
        sc.rect(screen, bg);

        // Top bar: Cancel + "page / total".
        button(&mut sc, cancel_btn, "Cancel", Font::Regular);
        sc.text(
            Rect::new(screen.w - MARGIN - 120, 4, 120, BTN_H),
            format!("{} / {}", page + 1, n_pages),
            Font::Regular,
            Color::Black,
            bg,
            Align::Right,
        );

        // Content.
        if page == 0 {
            let mut y = content.y;
            y = text_block(surf, &mut sc, content.x, y, content.w, intro_text, Font::Large, Align::Center);
            y += 8;
            text_block(surf, &mut sc, content.x, y, content.w, intro_subtext, Font::Regular, Align::Center);
        } else if page == last {
            text_block(surf, &mut sc, content.x, content.y, content.w, final_text, Font::Large, Align::Center);
        } else {
            let range = ranges[page - 1].clone();
            let mut y = content.y;
            for p in &pairs[range] {
                y = text_block(surf, &mut sc, content.x, y, content.w, &p.tag, Font::Bold, Align::Left);
                y = text_block(surf, &mut sc, content.x, y, content.w, &p.value, Font::Regular, Align::Left);
                y += 12;
            }
        }

        // Bottom bar: confirm on the last page, otherwise prev/next.
        if page == last {
            button(&mut sc, confirm_btn, final_button_text, Font::Bold);
        } else {
            if page > 0 {
                button(&mut sc, prev_btn, "Back", Font::Bold);
            }
            button(&mut sc, next_btn, "Next", Font::Bold);
        }
        surf.paint(&sc);

        loop {
            let event = get_event().await;
            let Some(p) = touch_release(&event) else {
                continue;
            };
            if cancel_btn.contains(p) {
                return false;
            }
            if page == last {
                if confirm_btn.contains(p) {
                    return true;
                }
            } else if next_btn.contains(p) {
                page += 1;
                break;
            } else if page > 0 && prev_btn.contains(p) {
                page -= 1;
                break;
            }
        }
    }
}

async fn review_pairs_two_button(
    surf: &mut Surface,
    intro_text: &str,
    intro_subtext: &str,
    pairs: &[TagValue],
    final_button_text: &str,
) -> bool {
    // Steps: intro, one per pair, a confirm step, a reject step.
    let n_pair_steps = pairs.len();
    let n_steps = n_pair_steps + 3;
    let confirm_step = n_pair_steps + 1;
    let reject_step = n_pair_steps + 2;

    let mut step = 0usize;
    loop {
        let mut sc = Scene::new();
        sc.rect(surf.screen(), Color::White);
        if step == 0 {
            draw_two_button_message(surf, &mut sc, intro_text, intro_subtext, step, n_steps);
        } else if step <= n_pair_steps {
            let p = &pairs[step - 1];
            draw_two_button_message(surf, &mut sc, &p.tag, &p.value, step, n_steps);
        } else if step == confirm_step {
            draw_two_button_choice(surf, &mut sc, final_button_text, Icon::Confirm, step, n_steps);
        } else {
            draw_two_button_choice(surf, &mut sc, "Reject", Icon::Reject, step, n_steps);
        }
        surf.paint(&sc);

        loop {
            let event = get_event().await;
            match nav_from_event(&event) {
                Some(Nav::Prev) if step > 0 => {
                    step -= 1;
                    break;
                }
                Some(Nav::Next) if step + 1 < n_steps => {
                    step += 1;
                    break;
                }
                Some(Nav::Select) => {
                    if step == confirm_step {
                        return true;
                    } else if step == reject_step {
                        return false;
                    }
                }
                _ => {}
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Two-button (Nano) screen drawing
// -----------------------------------------------------------------------------

// Draws left/right arrow hints in the top corners for steps that have a previous / next.
fn draw_nav_arrows(surf: &Surface, sc: &mut Scene, step: usize, n_steps: usize) {
    let w = surf.screen().w;
    let lh = line_h(surf, Font::Regular);
    if step > 0 {
        sc.text(Rect::new(0, 0, 12, lh), "<", Font::Bold, Color::Black, Color::White, Align::Center);
    }
    if step + 1 < n_steps {
        sc.text(Rect::new(w - 12, 0, 12, lh), ">", Font::Bold, Color::Black, Color::White, Align::Center);
    }
}

// A title + body text screen for the Nano, vertically stacked from the top.
fn draw_two_button_message(
    surf: &Surface,
    sc: &mut Scene,
    title: &str,
    body: &str,
    step: usize,
    n_steps: usize,
) {
    draw_nav_arrows(surf, sc, step, n_steps);
    let w = surf.screen().w;
    let mut y = 2;
    y = text_block(surf, sc, 0, y, w, title, Font::Bold, Align::Center);
    y += 2;
    text_block(surf, sc, 0, y, w, body, Font::Regular, Align::Center);
}

// A centered choice screen for the Nano ("Confirm" / "Reject"); both-button press selects it.
fn draw_two_button_choice(
    surf: &Surface,
    sc: &mut Scene,
    label: &str,
    _icon: Icon,
    step: usize,
    n_steps: usize,
) {
    draw_nav_arrows(surf, sc, step, n_steps);
    let w = surf.screen().w;
    let h = surf.screen().h;
    let lh = line_h(surf, Font::Bold);
    let y = ((h - lh) / 2).max(2);
    sc.text(Rect::new(0, y, w, lh), label, Font::Bold, Color::Black, Color::White, Align::Center);
}

// -----------------------------------------------------------------------------
// ux_idle
// -----------------------------------------------------------------------------

/// Draws the default idle ("ready") screen with the low-level primitives.
///
/// Unlike the old NBGL dashboard this neither registers an NBGL screen nor handles a Quit
/// gesture itself — it just paints and returns. With no NBGL screen active, raw input
/// events flow straight to the app, which is what lets the flows above work on Nano.
pub fn ux_idle() {
    let mut surf = Surface::new();
    let screen = surf.screen();
    let bg = Color::White;
    let content_w = screen.w - 2 * MARGIN;
    let lh = line_h(&surf, Font::Large);
    let y = ((screen.h - lh) / 2).max(MARGIN);

    let mut sc = Scene::new();
    sc.rect(screen, bg);
    let _ = content_w;
    sc.text(
        Rect::new(MARGIN, y, content_w, lh),
        "Application is ready",
        Font::Large,
        Color::Black,
        bg,
        Align::Center,
    );
    surf.paint(&sc);
}
