use core::cell::RefCell;
use core::ops::Range;

pub mod canvas;
pub mod screen;
/// An accelerated `embedded-graphics` `DrawTarget` (requires the `embedded-graphics` feature).
#[cfg(feature = "embedded-graphics")]
pub mod screen_target;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use common::ecall_constants::{DEVICE_PROPERTY_FEATURES, FEATURE_TOUCH};
pub use common::ux::{
    Action, Button, ButtonEvent, Deserializable, Event, EventCode, EventData, Icon, NavInfo,
    NavigationInfo, Page, PageContent, PageContentInfo, PressState, TagValue, TouchEvent,
    TouchState,
};

use crate::ecalls;
use crate::ui::{
    button, draw_icon_centered, icons, nav_arrows, nav_from_button, touch_release, wrap_lines,
    Align, Font, InputModel, Nav, Rect, Scene, Surface, NAV_ARROW_W,
};

// Returns true if the device supports the page UX model, false if it supports the step
// UX model. The page model is tied to touch input, so this is the FEATURE_TOUCH bit —
// a capability query that keeps working on devices that don't exist yet (the previous
// implementation matched on a device-id table and panicked on unknown ids).
pub fn has_page_api() -> bool {
    ecalls::get_device_property(DEVICE_PROPERTY_FEATURES) & FEATURE_TOUCH != 0
}

/// Blocks until an event is received, then returns it. The blocking happens inside the
/// `ecalls::get_event` call (it sleeps a ticker period); this just decodes the result.
pub async fn get_event() -> Event {
    let mut event_data = EventData::default();

    // On wasm the input queue can be empty between user inputs; suspend (yield back to the
    // JS step-driver, which renders the frame and collects input) until something is
    // queued, instead of busy-returning. Other targets read a single event directly.
    #[cfg(feature = "target_wasm")]
    let raw = loop {
        // SAFETY: event_data is a properly aligned, initialized EventData on the stack.
        let raw = unsafe { ecalls::get_event(&mut event_data) };
        if raw == crate::ecalls_wasm::NO_EVENT_CODE {
            crate::executor::yield_now().await;
            continue;
        }
        break raw;
    };
    #[cfg(not(feature = "target_wasm"))]
    // SAFETY: event_data is a properly aligned, initialized EventData on the stack.
    let raw = unsafe { ecalls::get_event(&mut event_data) };

    let event_code = EventCode::from(raw);
    match event_code {
        EventCode::Ticker => {
            // Give a chance to the executor to make progress on registered tasks
            crate::executor::yield_now().await;
            Event::Ticker
        }
        // SAFETY: each event code selects the matching union field, written by the VM.
        EventCode::Action => Event::Action(unsafe { event_data.action }),
        EventCode::Touch => Event::Touch(unsafe { event_data.touch }),
        EventCode::Button => Event::Button(unsafe { event_data.button }),
        EventCode::Unknown => Event::Unknown(unsafe { event_data.raw }),
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
    let th = surf.theme();
    let lh = line_h(surf, font);
    let lines = wrap_lines(text, w, |s| surf.measure(font, s).w as i32);
    let mut yy = y;
    for line in lines {
        sc.text(
            Rect::new(x, yy, w, lh),
            line,
            font,
            th.fg,
            th.bg,
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
    let th = surf.theme();
    let bg = th.bg;

    // The narrow Nano panel can't fit the large icon + Large font, so the two-button devices
    // use the smaller 20×20 icon (selected by pixel format in `icons::bitmap`), a tighter
    // icon/text gap, and the Regular font, so everything fits on the 64px-high screen.
    let (gap, font) = if surf.caps().input == InputModel::TwoButton {
        (4, Font::Regular)
    } else {
        (16, Font::Large)
    };

    let content_w = screen.w - 2 * MARGIN;
    // Pick the icon art matching the panel's native pixel format and theme polarity; a Gray4
    // blit on a 1bpp panel renders as gibberish on real hardware. `None` for icons we have no
    // art for.
    let bitmap = icons::bitmap(icon, surf.caps().pixel_format, th.is_light());
    let icon_h = bitmap.as_ref().map(|b| b.h + gap).unwrap_or(0);
    let text_h = block_height(&surf, content_w, text, font);
    let total = icon_h + text_h;
    let mut y = (screen.h - total) / 2;
    if y < MARGIN {
        y = MARGIN;
    }

    let mut sc = Scene::new();
    sc.rect(screen, bg);
    if let Some(b) = &bitmap {
        let r = draw_icon_centered(&mut sc, screen, b, y);
        y = r.bottom() + gap;
    }
    text_block(&surf, &mut sc, MARGIN, y, content_w, text, font, Align::Center);
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
    let th = surf.theme();
    let bg = th.bg;
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
        th.fg,
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
    let th = surf.theme();
    let bg = th.bg;
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
    button(&mut sc, th, reject_btn, reject, Font::Bold);
    button(&mut sc, th, confirm_btn, confirm, Font::Bold);
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
        sc.rect(surf.screen(), surf.theme().bg);
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
    let th = surf.theme();
    let bg = th.bg;
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
        button(&mut sc, th, cancel_btn, "Cancel", Font::Regular);
        sc.text(
            Rect::new(screen.w - MARGIN - 120, 4, 120, BTN_H),
            format!("{} / {}", page + 1, n_pages),
            Font::Regular,
            th.fg,
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
            button(&mut sc, th, confirm_btn, final_button_text, Font::Bold);
        } else {
            if page > 0 {
                button(&mut sc, th, prev_btn, "Back", Font::Bold);
            }
            button(&mut sc, th, next_btn, "Next", Font::Bold);
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

// Caches each character's pixel width for `font`. NBGL's `getTextWidth` sums per-character
// advances with no kerning, so a string's width is the sum of its characters' widths: we
// measure each distinct ASCII character once (via the `display_text_width` ECALL) and reuse
// it. This lets pagination compute split points from arithmetic alone — no per-probe ECALLs,
// and no repeated string measuring.
fn cached_char_width(surf: &Surface, font: Font) -> impl Fn(char) -> i32 + '_ {
    let cache = RefCell::new([-1i32; 128]); // per-ASCII-char width, -1 = not measured yet
    move |c: char| {
        let measure = || {
            let mut buf = [0u8; 4];
            surf.measure(font, c.encode_utf8(&mut buf)).w as i32
        };
        let i = c as usize;
        if i >= 128 {
            return measure();
        }
        let mut cache = cache.borrow_mut();
        if cache[i] < 0 {
            cache[i] = measure();
        }
        cache[i]
    }
}

// Splits `value` into the body text for each page so it fits in `per_page` lines at width
// `cw`, joined by an inline ellipsis at every cut: each page but the last ends with "..." and
// each page but the first begins with "...". A value that already fits comes back unchanged.
//
// The fast, approximate splitter: it character-wraps using a prefix-sum table of character
// widths — one pass, with no allocations or ECALLs in the inner loop — instead of re-wrapping
// candidate strings. For the long no-space values that actually paginate (keys, hex,
// addresses) character wrapping matches the renderer's wrapping exactly; for text with spaces
// a split may fall mid-word, which is acceptable here.
fn paginate_value(
    cw: i32,
    per_page: usize,
    value: &str,
    char_width: &impl Fn(char) -> i32,
) -> Vec<String> {
    let chars: Vec<char> = value.chars().collect();
    let n = chars.len();
    // cum[k] = pixel width of chars[0..k].
    let mut cum = Vec::with_capacity(n + 1);
    let mut acc = 0i32;
    cum.push(0);
    for &c in &chars {
        acc += char_width(c);
        cum.push(acc);
    }

    // The largest line end at or after `from` whose width fits `budget` (always at least one
    // character, so we keep advancing). Binary search over the cumulative widths.
    let line_end = |from: usize, budget: i32| -> usize {
        let limit = cum[from] + budget.max(1);
        let (mut lo, mut hi) = (from, n);
        while lo < hi {
            let mid = (lo + hi + 1) / 2;
            if cum[mid] <= limit {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        lo.max(from + 1).min(n)
    };

    // Fast path: the whole value character-wraps within `per_page` lines — one page, no marks.
    let mut probe = 0usize;
    let mut lines = 0usize;
    while probe < n {
        probe = line_end(probe, cw);
        lines += 1;
        if lines > per_page {
            break;
        }
    }
    if lines <= per_page {
        let mut single = Vec::new();
        single.push(String::from(value));
        return single;
    }

    let ell = 3 * char_width('.'); // width of the "..." marker
    let mut pages = Vec::new();
    let mut start = 0usize;
    let mut first = true;
    while start < n {
        // Fill up to `per_page` lines, reserving room for the leading ellipsis on the first
        // line of a continuation page and (conservatively) a trailing ellipsis on the last.
        let mut pos = start;
        for line in 0..per_page {
            if pos >= n {
                break;
            }
            let mut budget = cw;
            if line == 0 && !first {
                budget -= ell;
            }
            if line == per_page - 1 {
                budget -= ell;
            }
            pos = line_end(pos, budget);
        }
        let last = pos >= n;
        let mut s = String::new();
        if !first {
            s.push_str("...");
        }
        s.extend(chars[start..pos].iter());
        if !last {
            s.push_str("...");
        }
        pages.push(s);
        start = pos;
        first = false;
    }
    pages
}

async fn review_pairs_two_button(
    surf: &mut Surface,
    intro_text: &str,
    intro_subtext: &str,
    pairs: &[TagValue],
    final_button_text: &str,
) -> bool {
    // Build the flat list of message screens: the intro, then each pair. A value too long for
    // one screen is split across several pages titled "tag (i/n)" with an inline ellipsis at
    // each cut (see `paginate_value`); a value that fits keeps its bare tag as the title.
    let cw = surf.screen().w - 2 * NAV_ARROW_W;
    let lh_b = line_h(surf, Font::Bold);
    let lh_r = line_h(surf, Font::Regular);
    // Cache character widths up front so the pagination below works from arithmetic, with one
    // ECALL per distinct character instead of a width query per probe.
    let cwidth_b = cached_char_width(surf, Font::Bold);
    let cwidth_r = cached_char_width(surf, Font::Regular);
    let mut msgs: Vec<(String, String)> = Vec::new();
    msgs.push((String::from(intro_text), String::from(intro_subtext)));
    for p in pairs {
        // Reserve vertical space for the title at its worst-case width ("tag (NN/NN)"), so a
        // title that wraps to two lines can never push a value line off the bottom (which the
        // panel would clip — losing part of the value being reviewed).
        let title_w: i32 = format!("{} (99/99)", p.tag).chars().map(&cwidth_b).sum();
        let title_lines = ((title_w + cw - 1) / cw).max(1);
        let body_h = surf.screen().h - 2 - title_lines * lh_b - 2;
        let per_page = ((body_h / lh_r).max(1)) as usize;

        let pages = paginate_value(cw, per_page, &p.value, &cwidth_r);
        let n = pages.len();
        for (i, body) in pages.into_iter().enumerate() {
            let title = if n > 1 {
                format!("{} ({}/{})", p.tag, i + 1, n)
            } else {
                p.tag.clone()
            };
            msgs.push((title, body));
        }
    }
    // Release the borrow of `surf` the width caches held, so the render loop can repaint.
    drop(cwidth_r);
    drop(cwidth_b);

    // After the messages come the confirm and reject choice steps.
    let n_msgs = msgs.len();
    let confirm_step = n_msgs;
    let reject_step = n_msgs + 1;
    let n_steps = n_msgs + 2;

    let mut step = 0usize;
    loop {
        let mut sc = Scene::new();
        sc.rect(surf.screen(), surf.theme().bg);
        if step < n_msgs {
            let (title, body) = &msgs[step];
            draw_two_button_message(surf, &mut sc, title, body, step, n_steps);
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

// Draws the nav arrows for a step that has a previous (`step > 0`) / next (`step + 1 <
// n_steps`), via the shared [`nav_arrows`] helper so every two-button screen matches.
fn draw_nav_arrows(surf: &Surface, sc: &mut Scene, step: usize, n_steps: usize) {
    nav_arrows(surf, sc, step > 0, step + 1 < n_steps);
}

// A title + body text screen for the Nano, vertically stacked from the top. Content is inset
// from the side gutters so it stays clear of the nav arrows.
fn draw_two_button_message(
    surf: &Surface,
    sc: &mut Scene,
    title: &str,
    body: &str,
    step: usize,
    n_steps: usize,
) {
    draw_nav_arrows(surf, sc, step, n_steps);
    let cw = surf.screen().w - 2 * NAV_ARROW_W;
    let mut y = 2;
    y = text_block(surf, sc, NAV_ARROW_W, y, cw, title, Font::Bold, Align::Center);
    y += 2;
    text_block(surf, sc, NAV_ARROW_W, y, cw, body, Font::Regular, Align::Center);
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
    let th = surf.theme();
    let h = surf.screen().h;
    let cw = surf.screen().w - 2 * NAV_ARROW_W;
    let lh = line_h(surf, Font::Bold);
    let y = ((h - lh) / 2).max(2);
    sc.text(Rect::new(NAV_ARROW_W, y, cw, lh), label, Font::Bold, th.fg, th.bg, Align::Center);
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
    const READY: &str = "Application is ready";
    let mut surf = Surface::new();
    let screen = surf.screen();
    let bg = surf.theme().bg;
    let content_w = screen.w - 2 * MARGIN;
    // The Large title font is wider than the narrow two-button Nano panel, so "Application
    // is ready" overflows it (and Speculos rejects the off-screen blit). Fall back to the
    // Regular font there, and wrap so any title still fits the available width.
    let font = if surf.caps().input == InputModel::TwoButton {
        Font::Regular
    } else {
        Font::Large
    };
    let text_h = block_height(&surf, content_w, READY, font);
    let y = ((screen.h - text_h) / 2).max(MARGIN);

    let mut sc = Scene::new();
    sc.rect(screen, bg);
    text_block(&surf, &mut sc, MARGIN, y, content_w, READY, font, Align::Center);
    surf.paint(&sc);
}
