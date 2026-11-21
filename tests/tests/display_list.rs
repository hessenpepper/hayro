// Model-Written: claude-opus-5-5 (high effort, sp-worker-high) -- designed and written, 2026-11-21.
//! The display list (`signalpath` branch): a replay draws exactly what rendering the same
//! region directly draws, pixel for pixel, for a whole page and for an offset tile.

use crate::harness::runner::{Outcome, Test};
use crate::{interpreter_settings, try_load_pdf};
use hayro::hayro_interpret::util::RectExt;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::kurbo::{Affine, Rect};
use hayro::{DisplayList, render_region};

/// Committed PDFs covering text (TrueType, Type 1, CFF, CID), dashed and styled strokes,
/// clips, images and a drawing. None of them needs a fallback, so the replay is compared,
/// not a direct render with itself.
const FILES: &[&str] = &[
    "pdfs/custom/font_truetype_1.pdf",
    "pdfs/custom/font_type1_1.pdf",
    "pdfs/custom/font_type1_cff_1.pdf",
    "pdfs/custom/font_cid_1.pdf",
    "pdfs/custom/pdftc_900k_0156_page_2.pdf",
    "pdfs/custom/path_rendering_dash_array_0_phase.pdf",
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
                move || replay_matches_direct(file),
            )
        })
        .collect()
}

fn replay_matches_direct(file: &str) -> Outcome {
    let pdf = match try_load_pdf(file, "") {
        Ok(pdf) => pdf,
        Err(err) => return Outcome::Failed(err),
    };
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
        for (name, (scale, x, y, width, height)) in [("whole page", whole), ("tile", tile)] {
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
                let differing = a
                    .chunks(4)
                    .zip(b.chunks(4))
                    .filter(|(p, q)| p != q)
                    .count();
                return Outcome::Failed(format!(
                    "page {index}, {name}: {differing} pixels differ between the replay and \
                     the direct render"
                ));
            }
        }
    }

    Outcome::Passed
}
