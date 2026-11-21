// Model-Written: claude-opus-5-5 (high effort, sp-worker-high) -- designed and written, 2026-11-21.
//! Tests of the `signalpath` branch's own changes to the interpreter, rendered or read
//! through a device as an application sees them.

use crate::harness::runner::Test;
use crate::interpreter_settings;
use hayro::hayro_interpret::font::{DescriptorMetrics, Glyph, GlyphRun};
use hayro::hayro_interpret::util::RectExt;
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
    InterpreterCache, SoftMask, interpret_page,
};
use hayro::kurbo::{Affine, BezPath, Rect};
use hayro::render_region;
use hayro::vello_cpu::Pixmap;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro_syntax::Pdf;

pub(crate) fn tests() -> Vec<Test> {
    vec![
        Test::unit(
            "signalpath::a_dashed_path_starts_at_its_own_first_point",
            a_dashed_path_starts_at_its_own_first_point,
        ),
        Test::unit(
            "signalpath::descriptor_metrics_reach_the_device",
            descriptor_metrics_reach_the_device,
        ),
    ]
}

/// A one-page PDF of `width` by `height` points drawing `content`.
fn one_page(width: f32, height: f32, content: &[u8]) -> Vec<u8> {
    use pdf_writer::{Finish, Rect, Ref};

    let (catalog, tree, page, contents) = (Ref::new(1), Ref::new(2), Ref::new(3), Ref::new(4));
    let mut pdf = pdf_writer::Pdf::new();
    pdf.catalog(catalog).pages(tree);
    pdf.pages(tree).kids([page]).count(1);
    let mut p = pdf.page(page);
    p.media_box(Rect::new(0.0, 0.0, width, height));
    p.parent(tree);
    p.contents(contents);
    p.finish();
    pdf.stream(contents, content);
    pdf.finish()
}

/// Renders the first page at `scale` pixels per point, y down from the top.
fn render(bytes: Vec<u8>, scale: f64) -> Pixmap {
    let pdf = Pdf::new(bytes).unwrap();
    let page = &pdf.pages()[0];
    let crop = page.intersected_crop_box().to_kurbo();
    let transform = Affine::scale(scale) * Affine::new([1.0, 0.0, 0.0, -1.0, -crop.x0, crop.y1]);
    let (width, height) = (
        (crop.width() * scale).ceil() as u16,
        (crop.height() * scale).ceil() as u16,
    );
    render_region(
        page,
        &interpreter_settings(),
        transform,
        width,
        height,
        WHITE,
        &|| false,
    )
    .unwrap()
}

/// Whether the pixel under the point (`x`, `y`) of a 100-point-tall page rendered at `scale`
/// is painted.
fn painted(pixmap: &Pixmap, scale: f64, x: f64, y: f64) -> bool {
    let (px, py) = ((x * scale) as usize, ((100.0 - y) * scale) as usize);
    let i = (py * pixmap.width() as usize + px) * 4;
    pixmap.data_as_u8_slice()[i..i + 3] != [255, 255, 255]
}

/// A dash pattern runs from the path's first point in the path's own direction (ISO 32000
/// 8.4.3.6), so a dashed closed four-sided path keeps its path instead of being stroked as
/// the rectangle it outlines (`hayro-interpret`'s `stroke_path_impl`). The same outline drawn
/// from (180, 80) leftwards and as `re` from (20, 20) puts the dashes on the top edge in
/// different places: [20 10] from the path's start leaves (155, 80) in a gap and (165, 80)
/// in a dash for the first, and the opposite for the second (245 and 235 along `re`).
fn a_dashed_path_starts_at_its_own_first_point() {
    let style = b"0 0 0 RG 4 w [20 10] 0 d ";
    let drawn = [&style[..], b"180 80 m 20 80 l 20 20 l 180 20 l h S"].concat();
    let rect = [&style[..], b"20 20 160 60 re S"].concat();
    let scale = 2.0;
    let drawn = render(one_page(200.0, 100.0, &drawn), scale);
    let rect = render(one_page(200.0, 100.0, &rect), scale);

    assert!(
        painted(&drawn, scale, 165.0, 80.0),
        "a dash 15 from the path's start"
    );
    assert!(
        !painted(&drawn, scale, 155.0, 80.0),
        "a gap 25 from the path's start"
    );
    assert!(
        painted(&rect, scale, 155.0, 80.0),
        "a dash 245 along the rectangle"
    );
    assert!(
        !painted(&rect, scale, 165.0, 80.0),
        "a gap 235 along the rectangle"
    );
}

/// Collects `OutlineGlyph::descriptor_metrics` of every glyph, one list per glyph run.
#[derive(Default)]
struct MetricsDevice {
    runs: Vec<Vec<DescriptorMetrics>>,
}

impl<'a> Device<'a> for MetricsDevice {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(
        &mut self,
        _: f32,
        _: Option<SoftMask<'a>>,
        _: BlendMode,
        _: bool,
        _: bool,
    ) {
    }
    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, _: DrawProps<'a>, _: &DrawMode) {
        let metrics = run
            .glyphs()
            .iter()
            .filter_map(|g| match &**g {
                Glyph::Outline(o) => Some(o.descriptor_metrics()),
                Glyph::Type3(_) => None,
            })
            .collect();
        self.runs.push(metrics);
    }
    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}
}

/// A page showing text in three fonts: a Type 1 font whose `/FontDescriptor` gives Ascent
/// 777, Descent -333 and CapHeight 555; a Type 0 font whose descendant's gives Ascent 880
/// and Descent -120 and no CapHeight; and a Type 1 font with no descriptor.
fn three_fonts() -> Vec<u8> {
    use pdf_writer::{Finish, Name, Rect, Ref, Str};

    let mut pdf = pdf_writer::Pdf::new();
    pdf.catalog(Ref::new(1)).pages(Ref::new(2));
    pdf.pages(Ref::new(2)).kids([Ref::new(3)]).count(1);
    let mut p = pdf.page(Ref::new(3));
    p.media_box(Rect::new(0.0, 0.0, 200.0, 100.0));
    p.parent(Ref::new(2));
    p.contents(Ref::new(4));
    p.resources()
        .fonts()
        .pair(Name(b"F1"), Ref::new(5))
        .pair(Name(b"F2"), Ref::new(7))
        .pair(Name(b"F3"), Ref::new(10));
    p.finish();
    pdf.stream(
        Ref::new(4),
        b"BT /F1 12 Tf 20 70 Td (Ab) Tj ET \
          BT /F2 12 Tf 20 45 Td <00240025> Tj ET \
          BT /F3 12 Tf 20 20 Td (C) Tj ET",
    );

    let font = |pdf: &mut pdf_writer::Pdf, id: i32, subtype: &[u8]| {
        let mut f = pdf.indirect(Ref::new(id)).dict();
        f.pair(Name(b"Type"), Name(b"Font"));
        f.pair(Name(b"Subtype"), Name(subtype));
        f.pair(Name(b"BaseFont"), Name(b"Helvetica"));
        f.finish();
    };
    let descriptor = |pdf: &mut pdf_writer::Pdf, id: i32, metrics: &[(&[u8], f32)]| {
        let mut d = pdf.indirect(Ref::new(id)).dict();
        d.pair(Name(b"Type"), Name(b"FontDescriptor"));
        d.pair(Name(b"FontName"), Name(b"Helvetica"));
        d.pair(Name(b"Flags"), 32);
        d.pair(Name(b"ItalicAngle"), 0);
        d.pair(Name(b"StemV"), 80);
        d.insert(Name(b"FontBBox"))
            .array()
            .items([-166, -225, 1000, 931]);
        for &(key, value) in metrics {
            d.pair(Name(key), value);
        }
        d.finish();
    };

    {
        let mut f = pdf.indirect(Ref::new(5)).dict();
        f.pair(Name(b"Type"), Name(b"Font"));
        f.pair(Name(b"Subtype"), Name(b"Type1"));
        f.pair(Name(b"BaseFont"), Name(b"Helvetica"));
        f.pair(Name(b"FontDescriptor"), Ref::new(6));
    }
    descriptor(
        &mut pdf,
        6,
        &[
            (b"Ascent", 777.0),
            (b"Descent", -333.0),
            (b"CapHeight", 555.0),
        ],
    );
    {
        let mut f = pdf.indirect(Ref::new(7)).dict();
        f.pair(Name(b"Type"), Name(b"Font"));
        f.pair(Name(b"Subtype"), Name(b"Type0"));
        f.pair(Name(b"BaseFont"), Name(b"Helvetica"));
        f.pair(Name(b"Encoding"), Name(b"Identity-H"));
        f.insert(Name(b"DescendantFonts")).array().item(Ref::new(8));
    }
    {
        let mut f = pdf.indirect(Ref::new(8)).dict();
        f.pair(Name(b"Type"), Name(b"Font"));
        f.pair(Name(b"Subtype"), Name(b"CIDFontType2"));
        f.pair(Name(b"BaseFont"), Name(b"Helvetica"));
        f.pair(Name(b"FontDescriptor"), Ref::new(9));
        let mut info = f.insert(Name(b"CIDSystemInfo")).dict();
        info.pair(Name(b"Registry"), Str(b"Adobe"));
        info.pair(Name(b"Ordering"), Str(b"Identity"));
        info.pair(Name(b"Supplement"), 0);
    }
    descriptor(&mut pdf, 9, &[(b"Ascent", 880.0), (b"Descent", -120.0)]);
    font(&mut pdf, 10, b"Type1");
    pdf.finish()
}

/// `OutlineGlyph::descriptor_metrics` carries each font's own `/FontDescriptor` values to a
/// device (`hayro_font::Font::new` reads them, `Font::glyph` passes them on), the Type 0
/// font's from its descendant, and nothing for a font without a descriptor.
fn descriptor_metrics_reach_the_device() {
    let pdf = Pdf::new(three_fonts()).unwrap();
    let page = &pdf.pages()[0];
    let cache = InterpreterCache::new();
    let mut context = Context::new(
        Affine::IDENTITY,
        Rect::new(0.0, 0.0, 200.0, 100.0),
        &cache,
        page.xref(),
        interpreter_settings(),
    );
    let mut device = MetricsDevice::default();
    interpret_page(page, &mut context, &mut device);

    let simple = DescriptorMetrics {
        ascent: Some(777.0),
        descent: Some(-333.0),
        cap_height: Some(555.0),
    };
    let type0 = DescriptorMetrics {
        ascent: Some(880.0),
        descent: Some(-120.0),
        cap_height: None,
    };
    let want = [(simple, 2), (type0, 2), (DescriptorMetrics::default(), 1)];
    assert_eq!(device.runs.len(), want.len(), "one glyph run per font");
    for (run, (metrics, glyphs)) in device.runs.iter().zip(want) {
        assert_eq!(run.len(), glyphs, "every glyph is an outline glyph");
        assert!(
            run.iter().all(|m| *m == metrics),
            "{run:?} is not {metrics:?}"
        );
    }
}
