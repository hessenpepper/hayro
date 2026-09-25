// Model-Written: Claude Opus 5.5 (claude-opus-5-5), 2026-09-25
//! A display list (Signal Path's `signalpath` branch; not upstream hayro).
//!
//! [`render`](crate::render) interprets the whole page again for every call and draws every
//! drawing call, including those outside the viewport. For an application that renders one
//! page as many tiles, that dominates: Signal Path measured it at 12× `MuPDF` per 512 px tile.
//! [`DisplayList::record`] interprets the page once into drawing calls the list owns, each
//! with its bounding box, and [`DisplayList::replay`] draws only the calls that touch the
//! requested region, through the same renderer. It is byte-identical to rendering the region
//! directly ([`render_region`]).
//!
//! The list owns everything it holds, and is `Send + Sync`. Colours are stored as the RGBA
//! the renderer would use, glyphs as their outlines, and images decoded once at full
//! resolution. A few constructs keep state that a replay under another transform wouldn't
//! reproduce: pattern paints, soft masks, Type 3 glyphs, and stencil images painted with a
//! pattern. A page that uses any of them is recorded as [`DisplayList::fallback`], and its
//! replays render the region directly instead. That is always correct, only slower.

use crate::image::{Decoded, DecodedImage};
use crate::{GlobalState, RenderCache, Renderer};
use hayro_interpret::font::Glyph;
use hayro_interpret::hayro_syntax::page::Page;
use hayro_interpret::util::RectExt;
use hayro_interpret::{
    BlendMode, CacheKey, ClipPath, Context, Device, DrawMode, DrawProps, FillRule, Image,
    ImageDrawProps, InterpreterSettings, Paint, SoftMask, interpret_page,
};
use kurbo::{Affine, BezPath, Rect, Shape};
use rustc_hash::FxHashMap;
use std::sync::Arc;
use vello_cpu::Pixmap;
use vello_cpu::color::{AlphaColor, Srgb};

enum Item {
    Path(BezPath, Affine, [u8; 4], BlendMode, DrawMode),
    Rect(Rect, Affine, [u8; 4], BlendMode, DrawMode),
    Glyphs(
        Vec<(Arc<BezPath>, Affine)>,
        Affine,
        [u8; 4],
        BlendMode,
        DrawMode,
    ),
    Image(Arc<DecodedImage>, Affine, BlendMode),
    PushClip(ClipPath),
    PushClipRect(Rect),
    PopClip,
    PushGroup(f32, BlendMode),
    PopGroup,
}

/// A page's drawing calls, recorded once; see the module docs.
pub struct DisplayList {
    items: Vec<Item>,
    /// Each item's bounding box in the recording's space; structural items span everything.
    boxes: Vec<Rect>,
    /// The page's crop box in the recording's space, the clip every replay starts with.
    crop: BezPath,
    /// The transform from PDF user space to the recording's space.
    to_space: Affine,
    fallback: Option<&'static str>,
    images: usize,
}

const EVERYWHERE: Rect = Rect::new(-1e12, -1e12, 1e12, 1e12);

impl DisplayList {
    /// Interprets `page` once. `to_space` maps PDF user space to the space the list is
    /// recorded in, and replayed from. Signal Path uses un-rotated points, y down from the
    /// top of the crop box. `bounds` is the page's extent in that space.
    pub fn record(
        page: &Page<'_>,
        settings: &InterpreterSettings,
        to_space: Affine,
        bounds: Rect,
    ) -> Self {
        let cache = RenderCache::new();
        let mut crop = page.intersected_crop_box().to_kurbo().to_path(0.1);
        crop.apply_affine(to_space);
        let mut rec = Recorder {
            list: Self {
                items: Vec::new(),
                boxes: Vec::new(),
                crop,
                to_space,
                fallback: None,
                images: 0,
            },
            outlines: FxHashMap::default(),
        };
        let mut state = Context::new(
            to_space,
            bounds,
            &cache.interpreter_cache,
            page.xref(),
            settings.clone(),
        );
        interpret_page(page, &mut state, &mut rec);
        rec.list
    }

    /// Why the page can't be replayed from the list, if it can't; its replays then render
    /// the region directly.
    pub fn fallback(&self) -> Option<&'static str> {
        self.fallback
    }

    /// The number of recorded drawing calls.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether nothing was recorded.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The number of images the list holds decoded.
    pub fn images(&self) -> usize {
        self.images
    }

    /// Draws the region `transform` maps onto `0..width` × `0..height`, where `transform`
    /// maps the recording's space to pixels, on `background`. `None` if `cancelled` returns
    /// true; it is asked every 1,024 drawing calls. For a [`fallback`](Self::fallback) page,
    /// `page` and `settings` are used to render the region directly; otherwise they are
    /// ignored.
    #[allow(clippy::too_many_arguments)]
    pub fn replay(
        &self,
        page: &Page<'_>,
        settings: &InterpreterSettings,
        transform: Affine,
        width: u16,
        height: u16,
        background: AlphaColor<Srgb>,
        cancelled: &dyn Fn() -> bool,
    ) -> Option<Pixmap> {
        if self.fallback.is_some() {
            return render_region(
                page,
                settings,
                transform * self.to_space,
                width,
                height,
                background,
                cancelled,
            );
        }
        // The region in the recording's space, widened by two pixels: the renderer widens
        // hairlines to at least one pixel at replay time, which a box in points doesn't know.
        let inverse = transform.inverse();
        let px = 2.0 / transform.determinant().abs().sqrt().max(1e-9);
        let view = inverse
            .transform_rect_bbox(Rect::new(0.0, 0.0, width as f64, height as f64))
            .inflate(px, px);
        let cache = RenderCache::new();
        let global = GlobalState::new(&cache);
        let mut device = Renderer::new(width, height, vc_settings(), &global);
        let mut crop = self.crop.clone();
        crop.apply_affine(transform);
        device.push_clip_path(&ClipPath {
            path: crop,
            fill: FillRule::NonZero,
        });
        for (i, (item, bbox)) in self.items.iter().zip(&self.boxes).enumerate() {
            if i % 1024 == 0 && cancelled() {
                return None;
            }
            let visible = bbox.x1 >= view.x0
                && bbox.x0 <= view.x1
                && bbox.y1 >= view.y0
                && bbox.y0 <= view.y1;
            match item {
                Item::Path(p, t, rgba, blend, mode) => {
                    if visible {
                        device.draw_path_rgba(p, transform * *t, *rgba, *blend, mode);
                    }
                }
                Item::Rect(r, t, rgba, blend, mode) => {
                    if visible {
                        device.draw_rect_rgba(r, transform * *t, *rgba, *blend, mode);
                    }
                }
                Item::Glyphs(g, t, rgba, blend, mode) => {
                    if visible {
                        device.draw_outline_run(g, transform * *t, *rgba, *blend, mode);
                    }
                }
                Item::Image(img, t, blend) => {
                    if visible {
                        device.draw_decoded_image(img, transform * *t, *blend);
                    }
                }
                Item::PushClip(c) => {
                    let mut path = c.path.clone();
                    path.apply_affine(transform);
                    device.push_clip_path(&ClipPath { path, fill: c.fill });
                }
                Item::PushClipRect(r) => device.push_clip_rect(&transform.transform_rect_bbox(*r)),
                Item::PopClip => device.pop_clip(),
                Item::PushGroup(o, b) => device.push_transparency_group(*o, None, *b),
                Item::PopGroup => device.pop_transparency_group(),
            }
        }
        device.pop_clip();
        Some(finish(device, width, height, background))
    }
}

fn vc_settings() -> vello_cpu::RenderSettings {
    vello_cpu::RenderSettings {
        level: vello_cpu::Level::new(),
        num_threads: 0,
    }
}

fn finish(device: Renderer<'_>, width: u16, height: u16, background: AlphaColor<Srgb>) -> Pixmap {
    let mut pixmap = Pixmap::new(width, height);
    let mut resources = vello_cpu::Resources::default();
    device.ctx.render_with(
        &mut pixmap,
        &mut resources,
        vello_cpu::RasterizerSettings {
            target_init: vello_cpu::TargetInit::Clear(background),
            ..Default::default()
        },
    );
    pixmap
}

/// Renders a region of `page` directly, interpreting the whole page, as [`render`](crate::render)
/// does but with any transform: `transform` maps PDF user space to pixels, and the region
/// drawn is `0..width` × `0..height`. `None` if `cancelled` returns true; drawing stops then,
/// though interpretation runs to the end of the page.
pub fn render_region(
    page: &Page<'_>,
    settings: &InterpreterSettings,
    transform: Affine,
    width: u16,
    height: u16,
    background: AlphaColor<Srgb>,
    cancelled: &dyn Fn() -> bool,
) -> Option<Pixmap> {
    let cache = RenderCache::new();
    let mut state = Context::new(
        transform,
        Rect::new(0.0, 0.0, width as f64, height as f64),
        &cache.interpreter_cache,
        page.xref(),
        settings.clone(),
    );
    let global = GlobalState::new(&cache);
    let mut device = Renderer::new(width, height, vc_settings(), &global);
    let mut clip = page.intersected_crop_box().to_kurbo().to_path(0.1);
    clip.apply_affine(transform);
    device.push_clip_path(&ClipPath {
        path: clip,
        fill: FillRule::NonZero,
    });
    {
        let mut dev = Cancellable {
            inner: &mut device,
            cancelled,
            stopped: false,
        };
        interpret_page(page, &mut state, &mut dev);
        if dev.stopped {
            return None;
        }
    }
    device.pop_clip();
    Some(finish(device, width, height, background))
}

/// Passes everything through until `cancelled` returns true; after that only the clip and
/// group pushes and pops, so the stacks stay balanced.
struct Cancellable<'d, 'r, 'c> {
    inner: &'d mut Renderer<'r>,
    cancelled: &'c dyn Fn() -> bool,
    stopped: bool,
}

impl Cancellable<'_, '_, '_> {
    fn off(&mut self) -> bool {
        if !self.stopped && (self.cancelled)() {
            self.stopped = true;
        }
        self.stopped
    }
}

impl<'a> Device<'a> for Cancellable<'_, '_, '_> {
    fn draw_path(&mut self, path: &BezPath, props: DrawProps<'a>, mode: &DrawMode) {
        if !self.off() {
            Device::draw_path(self.inner, path, props, mode);
        }
    }
    fn draw_rect(&mut self, rect: &Rect, props: DrawProps<'a>, mode: &DrawMode) {
        if !self.off() {
            Device::draw_rect(self.inner, rect, props, mode);
        }
    }
    fn push_clip_path(&mut self, c: &ClipPath) {
        Device::push_clip_path(self.inner, c);
    }
    fn push_clip_rect(&mut self, r: &Rect) {
        Device::push_clip_rect(self.inner, r);
    }
    fn push_transparency_group(&mut self, o: f32, m: Option<SoftMask<'a>>, b: BlendMode) {
        Device::push_transparency_group(self.inner, o, m, b);
    }
    fn draw_glyph_run(
        &mut self,
        run: &hayro_interpret::font::GlyphRun<'_, 'a>,
        props: DrawProps<'a>,
        mode: &DrawMode,
    ) {
        if !self.off() {
            Device::draw_glyph_run(self.inner, run, props, mode);
        }
    }
    fn draw_image(&mut self, image: Image<'a, '_>, props: ImageDrawProps<'a>) {
        if !self.off() {
            Device::draw_image(self.inner, image, props);
        }
    }
    fn pop_clip(&mut self) {
        Device::pop_clip(self.inner);
    }
    fn pop_transparency_group(&mut self) {
        Device::pop_transparency_group(self.inner);
    }
}

struct Recorder {
    list: DisplayList,
    /// Glyph outlines by glyph identity, shared between runs as the renderer's cache is.
    outlines: FxHashMap<u128, Arc<BezPath>>,
}

impl Recorder {
    fn push(&mut self, item: Item, bbox: Rect) {
        if self.list.fallback.is_none() {
            self.list.items.push(item);
            self.list.boxes.push(bbox);
        }
    }

    /// Records the reason and drops what was recorded: replays of this page render directly.
    fn unsupported(&mut self, why: &'static str) {
        if self.list.fallback.is_none() {
            self.list.fallback = Some(why);
            self.list.items = Vec::new();
            self.list.boxes = Vec::new();
        }
    }

    /// The RGBA of a plain-colour paint with no soft mask, as the renderer would set it.
    fn plain(&mut self, props: &DrawProps<'_>) -> Option<[u8; 4]> {
        if props.soft_mask.is_some() {
            self.unsupported("a soft mask");
            return None;
        }
        match &props.paint {
            Paint::Color(c) => Some(c.to_rgba().to_rgba8()),
            Paint::Pattern(_) => {
                self.unsupported("a pattern paint");
                None
            }
        }
    }
}

fn pad(t: &Affine, mode: &DrawMode) -> f64 {
    let scale = t.determinant().abs().sqrt();
    match mode {
        DrawMode::Stroke(s) | DrawMode::FillAndStroke(_, s) => {
            s.line_width as f64 * scale / 2.0 + 1.0
        }
        _ => 1.0,
    }
}

impl<'a> Device<'a> for Recorder {
    fn draw_path(&mut self, path: &BezPath, props: DrawProps<'a>, mode: &DrawMode) {
        if matches!(mode, DrawMode::Invisible) {
            return;
        }
        let Some(rgba) = self.plain(&props) else {
            return;
        };
        let p = pad(&props.transform, mode);
        let bbox = props
            .transform
            .transform_rect_bbox(path.bounding_box())
            .inflate(p, p);
        self.push(
            Item::Path(
                path.clone(),
                props.transform,
                rgba,
                props.blend_mode,
                mode.clone(),
            ),
            bbox,
        );
    }

    fn draw_rect(&mut self, rect: &Rect, props: DrawProps<'a>, mode: &DrawMode) {
        if matches!(mode, DrawMode::Invisible) {
            return;
        }
        let Some(rgba) = self.plain(&props) else {
            return;
        };
        let p = pad(&props.transform, mode);
        let bbox = props.transform.transform_rect_bbox(*rect).inflate(p, p);
        self.push(
            Item::Rect(*rect, props.transform, rgba, props.blend_mode, mode.clone()),
            bbox,
        );
    }

    fn push_clip_path(&mut self, c: &ClipPath) {
        self.push(Item::PushClip(c.clone()), EVERYWHERE);
    }

    fn push_clip_rect(&mut self, r: &Rect) {
        self.push(Item::PushClipRect(*r), EVERYWHERE);
    }

    fn push_transparency_group(&mut self, o: f32, m: Option<SoftMask<'a>>, b: BlendMode) {
        if m.is_some() {
            self.unsupported("a group with a soft mask");
        }
        self.push(Item::PushGroup(o, b), EVERYWHERE);
    }

    fn draw_glyph_run(
        &mut self,
        run: &hayro_interpret::font::GlyphRun<'_, 'a>,
        props: DrawProps<'a>,
        mode: &DrawMode,
    ) {
        if matches!(mode, DrawMode::Invisible) {
            return;
        }
        let Some(rgba) = self.plain(&props) else {
            return;
        };
        let mut glyphs = Vec::with_capacity(run.glyphs().len());
        let mut bbox: Option<Rect> = None;
        for g in run.glyphs() {
            let Glyph::Outline(o) = &**g else {
                self.unsupported("a Type 3 glyph");
                return;
            };
            let outline = self
                .outlines
                .entry(o.identifier().cache_key())
                .or_insert_with(|| Arc::new(o.outline()))
                .clone();
            if !outline.elements().is_empty() {
                let b =
                    (props.transform * g.transform()).transform_rect_bbox(outline.bounding_box());
                bbox = Some(bbox.map_or(b, |a| a.union(b)));
            }
            glyphs.push((outline, g.transform()));
        }
        let p = pad(&props.transform, mode);
        let bbox = bbox.map_or(Rect::ZERO, |b| b.inflate(p, p));
        self.push(
            Item::Glyphs(
                glyphs,
                props.transform,
                rgba,
                props.blend_mode,
                mode.clone(),
            ),
            bbox,
        );
    }

    fn draw_image(&mut self, image: Image<'a, '_>, props: ImageDrawProps<'a>) {
        if props.soft_mask.is_some() {
            self.unsupported("an image with a soft mask");
            return;
        }
        if self.list.fallback.is_some() {
            return;
        }
        let bbox = props
            .transform
            .transform_rect_bbox(Rect::new(
                0.0,
                0.0,
                image.width() as f64,
                image.height() as f64,
            ))
            .inflate(1.0, 1.0);
        match DecodedImage::decode(&image) {
            Decoded::Image(d) => {
                self.list.images += 1;
                self.push(
                    Item::Image(Arc::new(d), props.transform, props.blend_mode),
                    bbox,
                );
            }
            Decoded::Nothing => {}
            Decoded::Unsupported => self.unsupported("a stencil image painted with a pattern"),
        }
    }

    fn pop_clip(&mut self) {
        self.push(Item::PopClip, EVERYWHERE);
    }

    fn pop_transparency_group(&mut self) {
        self.push(Item::PopGroup, EVERYWHERE);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_display_list_can_be_shared_between_threads() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<super::DisplayList>();
    }
}
