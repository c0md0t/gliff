//! Where a picture lands in the window, how its pixels get there, and how a
//! window point maps back to the remote's logical coordinates. All sizes
//! here are physical pixels.

/// The destination of a picture inside the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Fit `view` into a `window` sized area: at its own size, centred, when it
/// fits; otherwise shrunk to fit with its aspect ratio kept. The server
/// sizes its view to the window we asked for, so the first case is the
/// usual one and needs no resampling.
pub fn fit(view: (u32, u32), window: (u32, u32)) -> Option<Rect> {
    let (vw, vh) = view;
    let (ww, wh) = window;
    if vw == 0 || vh == 0 || ww == 0 || wh == 0 {
        return None;
    }
    let (width, height) = if vw <= ww && vh <= wh {
        (vw, vh)
    } else {
        let s = (ww as f64 / vw as f64).min(wh as f64 / vh as f64);
        (
            ((vw as f64 * s).round() as u32).clamp(1, ww),
            ((vh as f64 * s).round() as u32).clamp(1, wh),
        )
    };
    Some(Rect {
        x: (ww - width) / 2,
        y: (wh - height) / 2,
        width,
        height,
    })
}

/// Draw a packed BGRA `src` frame into `dst` (0RGB words, `stride` per row)
/// at `rect`, nearest-neighbour when the sizes differ. Everything outside
/// `rect` is cleared to black.
pub fn blit(dst: &mut [u32], stride: usize, src: &[u8], src_size: (u32, u32), rect: Rect) {
    dst.fill(0);
    let (sw, sh) = (src_size.0 as usize, src_size.1 as usize);
    if sw == 0 || sh == 0 || src.len() < sw * sh * 4 {
        return;
    }
    let (rx, ry) = (rect.x as usize, rect.y as usize);
    let (rw, rh) = (rect.width as usize, rect.height as usize);
    let rows = dst.len() / stride.max(1);
    let px = |p: &[u8]| u32::from_le_bytes([p[0], p[1], p[2], 0]);
    if (rw, rh) == (sw, sh) {
        for (y, src_row) in src.chunks_exact(sw * 4).take(sh).enumerate() {
            if ry + y >= rows {
                break;
            }
            let start = (ry + y) * stride + rx;
            let Some(out) = dst.get_mut(start..start + sw) else {
                break;
            };
            for (o, p) in out.iter_mut().zip(src_row.chunks_exact(4)) {
                *o = px(p);
            }
        }
        return;
    }
    let cols: Vec<usize> = (0..rw).map(|x| (x * sw / rw).min(sw - 1)).collect();
    for y in 0..rh.min(rows.saturating_sub(ry)) {
        let sy = (y * sh / rh).min(sh - 1);
        let src_row = &src[sy * sw * 4..(sy + 1) * sw * 4];
        let start = (ry + y) * stride + rx;
        let Some(out) = dst.get_mut(start..start + rw) else {
            break;
        };
        for (o, &sx) in out.iter_mut().zip(&cols) {
            *o = px(&src_row[sx * 4..sx * 4 + 4]);
        }
    }
}

/// Map a window point to the remote output's logical coordinates: through
/// `rect` to stream pixels, then divided by the stream scale. Points outside
/// the picture clamp to its edge.
pub fn to_remote(
    point: (f64, f64),
    rect: Rect,
    stream: (u32, u32),
    scale_milli: u32,
) -> (f64, f64) {
    if rect.width == 0 || rect.height == 0 {
        return (0.0, 0.0);
    }
    let (sw, sh) = (stream.0 as f64, stream.1 as f64);
    let px = ((point.0 - rect.x as f64) * sw / rect.width as f64).clamp(0.0, sw);
    let py = ((point.1 - rect.y as f64) * sh / rect.height as f64).clamp(0.0, sh);
    let scale = (scale_milli.max(1) as f64 / 1000.0).max(0.01);
    (px / scale, py / scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_view_fills_the_window() {
        let r = fit((1920, 1080), (1920, 1080)).unwrap();
        assert_eq!(
            r,
            Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080
            }
        );
    }

    #[test]
    fn smaller_view_is_centred_unscaled() {
        let r = fit((1600, 900), (1920, 1080)).unwrap();
        assert_eq!((r.x, r.y, r.width, r.height), (160, 90, 1600, 900));
    }

    #[test]
    fn larger_view_shrinks_keeping_aspect() {
        let r = fit((3840, 2160), (1920, 1200)).unwrap();
        assert_eq!((r.width, r.height), (1920, 1080));
        assert_eq!((r.x, r.y), (0, 60));
    }

    #[test]
    fn empty_sizes_have_no_fit() {
        assert!(fit((0, 0), (100, 100)).is_none());
        assert!(fit((10, 10), (0, 100)).is_none());
    }

    #[test]
    fn pointer_maps_through_letterbox_and_scale() {
        let r = fit((1600, 900), (1920, 1080)).unwrap();
        // Centre of the picture on a 1.25-scaled remote.
        let (x, y) = to_remote((960.0, 540.0), r, (1600, 900), 1250);
        assert_eq!((x, y), (640.0, 360.0));
        // Outside the picture clamps to its edge.
        assert_eq!(to_remote((0.0, 0.0), r, (1600, 900), 1000), (0.0, 0.0));
    }

    #[test]
    fn pointer_maps_a_reduced_stream_onto_the_view() {
        // A half-resolution stream stretched over a 1920x1080 view.
        let r = fit((1920, 1080), (1920, 1080)).unwrap();
        assert_eq!(
            to_remote((1920.0, 1080.0), r, (960, 540), 1000),
            (960.0, 540.0)
        );
    }

    #[test]
    fn blit_copies_bgra_as_xrgb() {
        // Two pixels: blue, then red.
        let src = [255, 0, 0, 255, 0, 0, 255, 255];
        let mut dst = vec![7u32; 4 * 2];
        let rect = Rect {
            x: 1,
            y: 1,
            width: 2,
            height: 1,
        };
        blit(&mut dst, 4, &src, (2, 1), rect);
        assert_eq!(dst, vec![0, 0, 0, 0, 0, 0x0000ff, 0xff0000, 0]);
    }

    #[test]
    fn blit_scales_nearest_neighbour() {
        let src = [1, 0, 0, 0, 2, 0, 0, 0];
        let mut dst = vec![0u32; 4];
        let rect = Rect {
            x: 0,
            y: 0,
            width: 4,
            height: 1,
        };
        blit(&mut dst, 4, &src, (2, 1), rect);
        assert_eq!(dst, vec![1, 1, 2, 2]);
    }
}
