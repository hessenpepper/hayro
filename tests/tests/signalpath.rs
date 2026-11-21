// Model-Written: claude-opus-5-5 (high effort, sp-worker-high) -- designed and written, 2026-11-21.
//! Tests of the `signalpath` branch's own changes to the interpreter, rendered or read
//! through a device as an application sees them.

use crate::harness::runner::Test;
use crate::interpreter_settings;
use hayro::hayro_interpret::util::RectExt;
use hayro::kurbo::Affine;
use hayro::render_region;
use hayro::vello_cpu::Pixmap;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro_syntax::Pdf;

pub(crate) fn tests() -> Vec<Test> {
    vec![Test::unit(
        "signalpath::a_dashed_path_starts_at_its_own_first_point",
        a_dashed_path_starts_at_its_own_first_point,
    )]
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
    let transform =
        Affine::scale(scale) * Affine::new([1.0, 0.0, 0.0, -1.0, -crop.x0, crop.y1]);
    let (width, height) = (
        (crop.width() * scale).ceil() as u16,
        (crop.height() * scale).ceil() as u16,
    );
    render_region(page, &interpreter_settings(), transform, width, height, WHITE, &|| false)
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

    assert!(painted(&drawn, scale, 165.0, 80.0), "a dash 15 from the path's start");
    assert!(!painted(&drawn, scale, 155.0, 80.0), "a gap 25 from the path's start");
    assert!(painted(&rect, scale, 155.0, 80.0), "a dash 245 along the rectangle");
    assert!(!painted(&rect, scale, 165.0, 80.0), "a gap 235 along the rectangle");
}
