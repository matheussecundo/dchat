//! Video grid geometry: fit N tiles into a box without scrolling.

/// Tiles keep a 16:9 frame; the video inside is letterboxed with `object-fit: contain`.
pub const TILE_ASPECT: f64 = 16.0 / 9.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridFit {
    pub cols: usize,
    pub tile_w: f64,
    pub tile_h: f64,
}

/// The column count giving the largest 16:9 tiles for `count` tiles inside a
/// `width` × `height` box with `gap` pixels between tiles. Every tile fits: nothing
/// overflows, so the grid never needs a scrollbar.
pub fn fit_grid(count: usize, width: f64, height: f64, gap: f64) -> GridFit {
    let count = count.max(1);
    let mut best = GridFit { cols: 1, tile_w: 0.0, tile_h: 0.0 };
    for cols in 1..=count {
        let rows = count.div_ceil(cols);
        let cell_w = (width - gap * (cols - 1) as f64) / cols as f64;
        let cell_h = (height - gap * (rows - 1) as f64) / rows as f64;
        if cell_w <= 0.0 || cell_h <= 0.0 {
            continue;
        }
        let tile_w = cell_w.min(cell_h * TILE_ASPECT).floor();
        if tile_w > best.tile_w {
            best = GridFit { cols, tile_w, tile_h: (tile_w / TILE_ASPECT).floor() };
        }
    }
    best
}

/// Where a video's picture sits inside a `box_w` × `box_h` element with
/// `object-fit: contain`: (left, top, width, height), letterbox bands excluded.
pub fn contain_rect(box_w: f64, box_h: f64, video_w: f64, video_h: f64) -> (f64, f64, f64, f64) {
    if box_w <= 0.0 || box_h <= 0.0 || video_w <= 0.0 || video_h <= 0.0 {
        return (0.0, 0.0, box_w.max(0.0), box_h.max(0.0));
    }
    let scale = (box_w / video_w).min(box_h / video_h);
    let (w, h) = (video_w * scale, video_h * scale);
    ((box_w - w) / 2.0, (box_h - h) / 2.0, w, h)
}

/// A point in the element (from its top-left corner) as fractions of the picture; outside
/// 0..=1 in the letterbox bands (the caller clamps, so the bands reach the screen's edges).
pub fn picture_fraction(x: f64, y: f64, picture: (f64, f64, f64, f64)) -> (f64, f64) {
    let (left, top, w, h) = picture;
    if w <= 0.0 || h <= 0.0 {
        return (0.0, 0.0);
    }
    ((x - left) / w, (y - top) / h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_letterboxed_picture_positions() {
        // A 16:9 screen in a square tile: bands above and below.
        let picture = contain_rect(800.0, 800.0, 1920.0, 1080.0);
        assert_eq!(picture, (0.0, 175.0, 800.0, 450.0));
        assert_eq!(picture_fraction(400.0, 400.0, picture), (0.5, 0.5));
        assert_eq!(picture_fraction(0.0, 175.0, picture), (0.0, 0.0));
        assert!(picture_fraction(10.0, 50.0, picture).1 < 0.0, "in the top band");
        // A portrait phone screen in a wide tile: bands left and right.
        let tall = contain_rect(1000.0, 500.0, 500.0, 1000.0);
        assert_eq!(tall, (375.0, 0.0, 250.0, 500.0));
        // Nothing known yet: the whole element.
        assert_eq!(contain_rect(300.0, 200.0, 0.0, 0.0), (0.0, 0.0, 300.0, 200.0));
        assert_eq!(picture_fraction(5.0, 5.0, (0.0, 0.0, 0.0, 0.0)), (0.0, 0.0));
    }

    fn fits(fit: GridFit, count: usize, width: f64, height: f64, gap: f64) -> bool {
        let rows = count.div_ceil(fit.cols);
        fit.tile_w * fit.cols as f64 + gap * (fit.cols - 1) as f64 <= width + 0.5
            && fit.tile_h * rows as f64 + gap * (rows - 1) as f64 <= height + 0.5
    }

    #[test]
    fn test_single_tile_is_limited_by_height_on_a_wide_stage() {
        // The scroll bug: one camera on a wide screen used to be 1300 px wide (730 px tall).
        let fit = fit_grid(1, 1318.0, 344.0, 8.0);
        assert_eq!(fit.cols, 1);
        assert!((fit.tile_h - 344.0).abs() <= 1.0, "{fit:?}");
        assert!(fits(fit, 1, 1318.0, 344.0, 8.0));
    }

    #[test]
    fn test_wide_stage_prefers_one_row() {
        let fit = fit_grid(3, 1318.0, 344.0, 8.0);
        assert_eq!(fit.cols, 3);
        assert!(fits(fit, 3, 1318.0, 344.0, 8.0));
    }

    #[test]
    fn test_narrow_stage_stacks_tiles() {
        let fit = fit_grid(3, 366.0, 600.0, 8.0);
        assert_eq!(fit.cols, 1);
        assert!(fits(fit, 3, 366.0, 600.0, 8.0));
    }

    #[test]
    fn test_every_count_fits_every_box() {
        for count in 1..=12 {
            for (w, h) in [(1318.0, 344.0), (366.0, 250.0), (1920.0, 1080.0), (800.0, 800.0), (200.0, 100.0)] {
                let fit = fit_grid(count, w, h, 8.0);
                assert!(fits(fit, count, w, h, 8.0), "{count} tiles in {w}x{h}: {fit:?}");
                assert!(fit.tile_w > 0.0, "{count} tiles in {w}x{h}");
            }
        }
    }

    #[test]
    fn test_degenerate_box() {
        let fit = fit_grid(4, 0.0, 0.0, 8.0);
        assert_eq!(fit.tile_w, 0.0);
    }
}
