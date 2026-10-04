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

#[cfg(test)]
mod tests {
    use super::*;

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
