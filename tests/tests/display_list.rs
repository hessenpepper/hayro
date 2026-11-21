// Model-Written: claude-opus-5-5 (high effort, sp-worker-high) -- designed and written, 2026-11-21.
//! The display list (`signalpath` branch): a replay draws exactly what rendering the same
//! region directly draws, pixel for pixel, for a whole page and for an offset tile.

use crate::harness::runner::{Outcome, Test};
use crate::{interpreter_settings, try_load_pdf};
use hayro::hayro_interpret::util::RectExt;
use hayro::kurbo::{Affine, Rect};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{DisplayList, render_region};
use hayro_syntax::Pdf;

/// Committed PDFs covering text (TrueType, Type 1, CFF, CID), stroke styles, clips, an
/// inline image and a drawing. None of them needs a fallback, so the replay is compared, not
/// a direct render with itself (`replay_matches_direct` fails a page that falls back).
const FILES: &[&str] = &[
    "pdfs/custom/font_truetype_1.pdf",
    "pdfs/custom/font_type1_1.pdf",
    "pdfs/custom/font_type1_cff_1.pdf",
    "pdfs/custom/font_cid_1.pdf",
    "pdfs/custom/pdftc_900k_0156_page_2.pdf",
    "pdfs/custom/stroke_properties.pdf",
    "pdfs/custom/clip_path_nested.pdf",
    "pdfs/custom/image_inline_2.pdf",
    "pdfs/custom/integration_diagram.pdf",
];

pub(crate) fn tests() -> Vec<Test> {
    FILES
        .iter()
        .map(|&file| {
            let stem = file
                .rsplit('/')
                .next()
                .and_then(|name| name.strip_suffix(".pdf"))
                .unwrap_or(file);
            Test::new(
                format!("display_list::{stem}"),
                vec![file.to_string()],
                move || match try_load_pdf(file, "") {
                    Ok(pdf) => replay_matches_direct(&pdf, &[]),
                    Err(err) => Outcome::Failed(err),
                },
            )
        })
        .chain([
            Test::new("display_list::dashed_paths", vec![], || {
                generated(dashed_paths(), &[])
            }),
            Test::new("display_list::miter_tip_and_square_cap", vec![], || {
                // Each tile starts just past what a box padded by half the line width
                // reaches, and holds part of the miter's tip or the cap's corner.
                generated(
                    miter_tip_and_square_cap(),
                    &[
                        ("miter tip", (3.0, 438.0, 210.0, 48, 48)),
                        ("square cap corner", (3.0, 756.0, 126.0, 48, 48)),
                    ],
                )
            }),
        ])
        .collect()
}

/// A region to compare: the scale from the recording's units to pixels, the region's top
/// left corner in pixels, and its width and height.
type Region = (f64, f64, f64, u16, u16);

fn generated(bytes: Vec<u8>, extra: &[(&str, Region)]) -> Outcome {
    match Pdf::new(bytes) {
        Ok(pdf) => replay_matches_direct(&pdf, extra),
        Err(err) => Outcome::Failed(format!("couldn't load the generated PDF: {err:?}")),
    }
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

/// On a 300 × 120 page: a 4-wide closed path with a sharp miter join (limit 10) whose tip
/// reaches 16.8 past the vertex at (140, 42), and a 40-wide square-capped diagonal whose
/// cap corner reaches 28.3 past its end (230, 70) in x.
fn miter_tip_and_square_cap() -> Vec<u8> {
    use pdf_writer::Content;
    use pdf_writer::types::{LineCapStyle, LineJoinStyle};

    let mut content = Content::new();
    content.set_stroke_rgb(0.1, 0.1, 0.1);
    content
        .set_line_width(4.0)
        .set_line_cap(LineCapStyle::ButtCap)
        .set_line_join(LineJoinStyle::MiterJoin)
        .set_miter_limit(10.0);
    content
        .move_to(40.0, 30.0)
        .line_to(140.0, 42.0)
        .line_to(40.0, 54.0)
        .close_and_stroke();
    content
        .set_line_width(40.0)
        .set_line_cap(LineCapStyle::ProjectingSquareCap);
    content.move_to(180.0, 20.0).line_to(230.0, 70.0).stroke();
    one_page(300.0, 120.0, &content.finish())
}

/// A page of dashed strokes: a closed rectangle (`re`), a closed four-sided path drawn the
/// other way from another corner, a curve, a sub-pixel dash period and a filled rectangle.
fn dashed_paths() -> Vec<u8> {
    use pdf_writer::Content;

    let mut content = Content::new();
    content.set_stroke_rgb(0.8, 0.1, 0.1).set_line_width(2.0);
    content.set_dash_pattern([6.0, 3.0], 1.0);
    content.rect(20.0, 20.0, 160.0, 70.0).stroke();
    content.set_dash_pattern([9.0, 2.0, 1.0, 2.0], 4.0);
    content
        .move_to(180.0, 110.0)
        .line_to(20.0, 110.0)
        .line_to(20.0, 180.0)
        .line_to(180.0, 180.0)
        .close_and_stroke();
    content.set_dash_pattern([4.0], 0.0);
    content
        .move_to(30.0, 130.0)
        .cubic_to(60.0, 175.0, 140.0, 85.0, 170.0, 130.0)
        .stroke();
    content.set_dash_pattern([0.02, 0.02], 0.0);
    content.move_to(30.0, 100.0).line_to(170.0, 100.0).stroke();
    content.set_fill_rgb(0.1, 0.3, 0.8);
    content.rect(90.0, 40.0, 30.0, 30.0).fill_nonzero();
    one_page(200.0, 200.0, &content.finish())
}

/// Replays every page for the whole page, an offset tile and the `extra` regions, and
/// compares each with `render_region`, byte for byte. Fails a page that falls back, a whole
/// page that renders blank, and an extra region the direct render leaves blank (it would
/// prove nothing).
fn replay_matches_direct(pdf: &Pdf, extra: &[(&str, Region)]) -> Outcome {
    let settings = interpreter_settings();
    let never = || false;

    for (index, page) in pdf.pages().iter().enumerate() {
        // Un-rotated points, y down from the top of the crop box, as Signal Path records.
        let crop = page.intersected_crop_box().to_kurbo();
        let to_space = Affine::new([1.0, 0.0, 0.0, -1.0, -crop.x0, crop.y1]);
        let bounds = Rect::new(0.0, 0.0, crop.width(), crop.height());
        let list = DisplayList::record(page, &settings, to_space, bounds);
        if let Some(why) = list.fallback() {
            return Outcome::Failed(format!("page {index} falls back: {why}"));
        }

        let whole = (
            1.5,
            0.0,
            0.0,
            (crop.width() * 1.5).ceil() as u16,
            (crop.height() * 1.5).ceil() as u16,
        );
        let tile = (3.0, 37.25, 53.5, 128, 96);
        let regions = [("whole page", whole), ("tile", tile)];
        for (i, &(name, (scale, x, y, width, height))) in regions.iter().chain(extra).enumerate() {
            let transform = Affine::translate((-x, -y)) * Affine::scale(scale);
            let replayed = list
                .replay(page, &settings, transform, width, height, WHITE, &never)
                .expect("not canceled");
            let direct = render_region(
                page,
                &settings,
                transform * to_space,
                width,
                height,
                WHITE,
                &never,
            )
            .expect("not canceled");
            let (a, b) = (replayed.data_as_u8_slice(), direct.data_as_u8_slice());
            if a != b {
                let differing = a.chunks(4).zip(b.chunks(4)).filter(|(p, q)| p != q).count();
                return Outcome::Failed(format!(
                    "page {index}, {name}: {differing} pixels differ between the replay and \
                     the direct render"
                ));
            }
            // Every region but the offset tile (index 1) must show something.
            if i != 1 && b.chunks(4).all(|p| p == [255, 255, 255, 255]) {
                return Outcome::Failed(format!("page {index}, {name}: renders blank"));
            }
        }
    }

    Outcome::Passed
}
