//! A retained [`Scene`] of semantic nodes and a [`render_diff`] that redraws only what
//! changed.
//!
//! Immediate-mode toolkits re-emit (and on Vanadium, re-rasterize) the whole screen every
//! frame. Here the app rebuilds a `Scene` each update and `render_diff` compares it to the
//! previous one: it computes the **damage region** (the union of the boxes of nodes that
//! changed) and redraws only the nodes overlapping it, clipped to that region. So a counter
//! tick repaints one rectangle, not the screen. The diff is pure logic, unit-tested below.

use alloc::string::String;
use alloc::vec::Vec;

use common::ecall_constants::PixelFormat;

use super::backend::Renderer;
use super::{Align, Color, ContentHint, Font, Rect};

/// A single semantic drawing primitive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// A solid filled rectangle.
    Rect { area: Rect, color: Color },
    /// A line of text on a known background `bg` (so it can be cleared before redrawing).
    Text {
        area: Rect,
        text: String,
        font: Font,
        color: Color,
        bg: Color,
        align: Align,
    },
    /// A bitmap icon, blitted as a whole. `pixels` is packed in `format` for an
    /// `area.w × area.h` image; the host draws it natively (`nbgl_frontDrawImage`), so the
    /// only guest cost is shipping the (small) packed bytes once through the blit ECALL.
    Icon {
        area: Rect,
        pixels: &'static [u8],
        format: PixelFormat,
    },
}

impl Node {
    /// The node's bounding box.
    pub fn area(&self) -> Rect {
        match self {
            Node::Rect { area, .. } | Node::Text { area, .. } | Node::Icon { area, .. } => *area,
        }
    }
}

/// A flat, ordered list of nodes (drawn back-to-front).
#[derive(Debug, Clone, Default)]
pub struct Scene {
    pub nodes: Vec<Node>,
}

impl Scene {
    pub fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    pub fn clear(&mut self) {
        self.nodes.clear();
    }

    /// Appends a filled rectangle.
    pub fn rect(&mut self, area: Rect, color: Color) {
        self.nodes.push(Node::Rect { area, color });
    }

    /// Appends a line of text drawn over background `bg`.
    pub fn text(
        &mut self,
        area: Rect,
        text: impl Into<String>,
        font: Font,
        color: Color,
        bg: Color,
        align: Align,
    ) {
        self.nodes.push(Node::Text {
            area,
            text: text.into(),
            font,
            color,
            bg,
            align,
        });
    }

    /// Appends a bitmap icon occupying `area`, with `pixels` packed in `format`.
    pub fn icon(&mut self, area: Rect, pixels: &'static [u8], format: PixelFormat) {
        self.nodes.push(Node::Icon {
            area,
            pixels,
            format,
        });
    }
}

// Redraws `n`, clipped to the damage region `clip`. Rectangles are clipped exactly; text
// clears its (clipped) background then draws — the renderer clips the glyphs to the text
// box, so a partially-damaged label is handled without stale pixels.
fn draw_clipped(n: &Node, r: &mut impl Renderer, clip: Rect) {
    let a = n.area().intersect(&clip);
    if a.is_empty() {
        return;
    }
    match n {
        Node::Rect { color, .. } => r.fill_rect(a, *color),
        Node::Text {
            area,
            text,
            font,
            color,
            bg,
            align,
        } => {
            r.fill_rect(a, *bg);
            r.text(*area, text, *font, *color, *bg, *align);
        }
        // The blit op takes a whole bitmap, so an icon overlapping the damage region is
        // redrawn in full (`*area`, not the clipped `a`). Icons here are small and static
        // and sit away from changing widgets, so they are rarely in any damage region.
        Node::Icon {
            area,
            pixels,
            format,
        } => r.blit(*area, pixels, *format),
    }
}

/// Draws `next` against `prev`, redrawing only the nodes overlapping what changed.
///
/// Returns the [`ContentHint`] to pass to [`Renderer::present`], or `None` if nothing
/// changed (the caller should then skip the panel refresh entirely). If the node *count*
/// differs (a structural change), the whole scene is redrawn and `FullScreen` is returned.
pub fn render_diff(prev: &Scene, next: &Scene, r: &mut impl Renderer) -> Option<ContentHint> {
    if prev.nodes.len() != next.nodes.len() {
        for n in &next.nodes {
            draw_clipped(n, r, n.area());
        }
        return Some(ContentHint::FullScreen);
    }

    // Damage region: the union of the boxes of every node that changed (old and new), so
    // a node that moved or shrank erases its previous footprint too.
    let mut damage = Rect::new(0, 0, 0, 0);
    let mut text_only = true;
    let mut any = false;
    for (a, b) in prev.nodes.iter().zip(&next.nodes) {
        if a != b {
            any = true;
            damage = damage.union(&a.area()).union(&b.area());
            if !matches!(a, Node::Text { .. }) || !matches!(b, Node::Text { .. }) {
                text_only = false;
            }
        }
    }
    if !any {
        return None;
    }

    // Redraw, in z-order, every node overlapping the damage region, clipped to it — so the
    // background and any fixed chrome under a moved node are restored.
    for n in &next.nodes {
        if n.area().intersects(&damage) {
            draw_clipped(n, r, damage);
        }
    }

    Some(if text_only {
        ContentHint::Text
    } else {
        ContentHint::Graphics
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::caps::{Capabilities, FontMetrics, InputModel};
    use crate::ui::{Renderer, Size};
    use common::ecall_constants::PixelFormat;

    // A renderer that records the fills/texts it receives, for asserting what got redrawn.
    struct RecordingRenderer {
        caps: Capabilities,
        fills: Vec<(Rect, Color)>,
        texts: Vec<(Rect, String)>,
        presented: Option<ContentHint>,
    }

    impl RecordingRenderer {
        fn new() -> Self {
            let fm = FontMetrics {
                height: 15,
                line_height: 17,
            };
            Self {
                caps: Capabilities {
                    size: Size::new(400, 672),
                    pixel_format: PixelFormat::Gray4,
                    native_text: true,
                    input: InputModel::Pointer,
                    partial_refresh: true,
                    fast_mono_refresh: true,
                    granularity: common::ecall_constants::DisplayGranularity {
                        x: 1,
                        y: 4,
                        w: 1,
                        h: 4,
                    },
                    max_text_len: common::ecall_constants::DISPLAY_MAX_TEXT_LEN,
                    fonts: [fm, fm, fm],
                },
                fills: Vec::new(),
                texts: Vec::new(),
                presented: None,
            }
        }
    }

    impl Renderer for RecordingRenderer {
        fn caps(&self) -> &Capabilities {
            &self.caps
        }
        fn measure(&self, font: Font, text: &str) -> Size {
            // 9px per char, font height from caps.
            Size::new(text.chars().count() as u32 * 9, self.caps.font(font).height as u32)
        }
        fn fill_rect(&mut self, area: Rect, color: Color) {
            self.fills.push((area, color));
        }
        fn text(&mut self, area: Rect, text: &str, _f: Font, _c: Color, _bg: Color, _a: Align) {
            self.texts.push((area, text.into()));
        }
        fn blit(&mut self, _area: Rect, _px: &[u8], _fmt: PixelFormat) {}
        fn present(&mut self, hint: ContentHint) {
            self.presented = Some(hint);
        }
    }

    fn label(area: Rect, s: &str) -> Node {
        Node::Text {
            area,
            text: s.into(),
            font: Font::Regular,
            color: Color::Black,
            bg: Color::White,
            align: Align::Left,
        }
    }

    #[test]
    fn first_paint_is_fullscreen_and_draws_everything() {
        let prev = Scene::new();
        let mut next = Scene::new();
        next.rect(Rect::new(0, 0, 400, 672), Color::White);
        next.text(Rect::new(10, 10, 200, 20), "hello", Font::Large, Color::Black, Color::White, Align::Left);

        let mut r = RecordingRenderer::new();
        let hint = render_diff(&prev, &next, &mut r);

        assert_eq!(hint, Some(ContentHint::FullScreen));
        assert_eq!(r.fills.len(), 2); // bg rect + text bg clear
        assert_eq!(r.texts.len(), 1);
    }

    #[test]
    fn unchanged_scene_redraws_nothing() {
        let mut a = Scene::new();
        a.rect(Rect::new(0, 0, 400, 672), Color::White);
        a.nodes.push(label(Rect::new(10, 10, 200, 20), "counter: 0"));
        let b = a.clone();

        let mut r = RecordingRenderer::new();
        let hint = render_diff(&a, &b, &mut r);

        assert_eq!(hint, None);
        assert!(r.fills.is_empty());
        assert!(r.texts.is_empty());
    }

    #[test]
    fn text_only_change_redraws_just_that_label_with_text_hint() {
        let bg = Node::Rect {
            area: Rect::new(0, 0, 400, 672),
            color: Color::White,
        };
        let mut prev = Scene::new();
        prev.nodes.push(bg.clone());
        prev.nodes.push(label(Rect::new(10, 100, 120, 20), "counter: 0"));
        prev.nodes.push(label(Rect::new(10, 200, 120, 20), "fixed"));

        let mut next = Scene::new();
        next.nodes.push(bg);
        next.nodes.push(label(Rect::new(10, 100, 120, 20), "counter: 1"));
        next.nodes.push(label(Rect::new(10, 200, 120, 20), "fixed"));

        let mut r = RecordingRenderer::new();
        let hint = render_diff(&prev, &next, &mut r);

        assert_eq!(hint, Some(ContentHint::Text));
        // Only the changed label's row was touched (its bg clear + text); the full-screen
        // bg is clipped to the damage region, and the other label is untouched.
        assert!(r.texts.iter().any(|(_, s)| s == "counter: 1"));
        assert!(!r.texts.iter().any(|(_, s)| s == "fixed"));
        // Every redraw stayed within the changed label's row.
        let damage = Rect::new(10, 100, 120, 20);
        for (area, _) in &r.fills {
            assert!(area.intersects(&damage));
            assert!(area.y >= 100 && area.bottom() <= 120);
        }
    }

    #[test]
    fn rect_change_yields_graphics_hint() {
        let mut prev = Scene::new();
        prev.rect(Rect::new(0, 0, 400, 672), Color::White);
        prev.rect(Rect::new(10, 10, 40, 40), Color::White); // checkbox unchecked

        let mut next = Scene::new();
        next.rect(Rect::new(0, 0, 400, 672), Color::White);
        next.rect(Rect::new(10, 10, 40, 40), Color::Black); // checked

        let mut r = RecordingRenderer::new();
        let hint = render_diff(&prev, &next, &mut r);
        assert_eq!(hint, Some(ContentHint::Graphics));
    }
}
