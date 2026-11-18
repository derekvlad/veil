//! Geometry for one picture spread across several monitors.
//!
//! Every monitor still gets its own layer surface and its own mpv instance.
//! The instances are told to zoom into the shared canvas: the same video is
//! scaled to cover the bounding box of all monitors, then each monitor pans to
//! its own slice. The maths mirrors mpv's `src_dst_split_scaling()`: `pan` is
//! a fraction of the zoomed video rectangle, `zoom` is log2 of the scale.
//!
//! All coordinates are logical Wayland coordinates (the same ones
//! `wl_output.geometry` reports).

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self {
            x: x as f64,
            y: y as f64,
            w: w as f64,
            h: h as f64,
        }
    }

    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn is_valid(&self) -> bool {
        self.w > 0.0 && self.h > 0.0
    }
}

/// mpv properties needed to show one slice of the shared picture.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Transform {
    /// `video-zoom`: log2 of the extra scale.
    pub zoom: f64,
    /// `video-pan-x`: fraction of the zoomed video width.
    pub pan_x: f64,
    /// `video-pan-y`: fraction of the zoomed video height.
    pub pan_y: f64,
}

impl Transform {
    pub fn is_identity(&self) -> bool {
        self.zoom.abs() < 1e-6 && self.pan_x.abs() < 1e-6 && self.pan_y.abs() < 1e-6
    }

    /// Compare with a tolerance that is tighter than one screen pixel.
    pub fn approx_eq(&self, other: &Self) -> bool {
        (self.zoom - other.zoom).abs() < 1e-4
            && (self.pan_x - other.pan_x).abs() < 1e-6
            && (self.pan_y - other.pan_y).abs() < 1e-6
    }
}

/// Bounding box of the given rectangles. Returns `None` when nothing is
/// visible.
pub fn bounding_box(rects: impl IntoIterator<Item = Rect>) -> Option<Rect> {
    let mut iter = rects.into_iter().filter(|rect| rect.is_valid());
    let first = iter.next()?;
    let (mut x0, mut y0) = (first.x, first.y);
    let (mut x1, mut y1) = (first.x + first.w, first.y + first.h);
    for rect in iter {
        x0 = x0.min(rect.x);
        y0 = y0.min(rect.y);
        x1 = x1.max(rect.x + rect.w);
        y1 = y1.max(rect.y + rect.h);
    }
    Some(Rect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    })
}

/// How to present `canvas` on one `output`, given the source aspect ratio.
///
/// The picture is assumed to be in `fill` mode: it covers the output first,
/// then this transform scales it up to cover the whole canvas and shifts it so
/// that the output shows its own part.
pub fn transform(canvas: Rect, output: Rect, aspect: f64) -> Transform {
    if !canvas.is_valid() || !output.is_valid() || !(aspect > 0.0) {
        return Transform::default();
    }

    // Size of the picture when it covers the output (mpv fill + panscan=1).
    let display_w = output.w.max(output.h * aspect);
    let display_h = display_w / aspect;

    // Scale needed to cover the canvas as well.
    let scale = (canvas.w / display_w).max(canvas.h / display_h);
    let scale = scale.max(1.0);

    let (canvas_x, canvas_y) = canvas.center();
    let (output_x, output_y) = output.center();

    Transform {
        zoom: scale.log2(),
        pan_x: -(output_x - canvas_x) / (scale * display_w),
        pan_y: -(output_y - canvas_y) / (scale * display_h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounding_box_of_two_screens() {
        let left = Rect::new(-2743, 117, 1920, 1080);
        let right = Rect::new(-823, 117, 1920, 1080);
        let canvas = bounding_box([left, right]).unwrap();
        assert_eq!(canvas, Rect::new(-2743, 117, 3840, 1080));
    }

    #[test]
    fn one_screen_is_identity() {
        let screen = Rect::new(0, 0, 1920, 1080);
        let transform = transform(screen, screen, 16.0 / 9.0);
        assert!(transform.approx_eq(&Transform::default()));
    }

    #[test]
    fn two_screens_zoom_by_two_and_split() {
        let left = Rect::new(0, 0, 1920, 1080);
        let right = Rect::new(1920, 0, 1920, 1080);
        let canvas = bounding_box([left, right]).unwrap();

        let left_t = transform(canvas, left, 16.0 / 9.0);
        let right_t = transform(canvas, right, 16.0 / 9.0);
        assert!((left_t.zoom - 1.0).abs() < 1e-9);
        assert!((right_t.zoom - 1.0).abs() < 1e-9);

        // Left screen looks at the left half: shift the 2x picture right.
        assert!((left_t.pan_x - 0.25).abs() < 1e-9);
        assert!((right_t.pan_x + 0.25).abs() < 1e-9);
        assert_eq!(left_t.pan_y, 0.0);
        assert_eq!(right_t.pan_y, 0.0);
    }

    #[test]
    fn taller_canvas_keeps_width_covered() {
        let top = Rect::new(0, 0, 1920, 1080);
        let bottom = Rect::new(0, 1080, 1920, 1080);
        let canvas = bounding_box([top, bottom]).unwrap();
        let t = transform(canvas, bottom, 16.0 / 9.0);
        // The 16:9 picture is filled to 1920x1080 and then zoomed 2x; the
        // bottom screen shows the lower slice, i.e. the picture moves up.
        assert!((t.zoom - 1.0).abs() < 1e-9);
        assert!((t.pan_y + 0.25).abs() < 1e-9);
        assert_eq!(t.pan_x, 0.0);
    }

    #[test]
    fn panoramic_source_covers_the_canvas_without_distortion() {
        let left = Rect::new(0, 0, 1920, 1080);
        let right = Rect::new(1920, 0, 1920, 1080);
        let canvas = bounding_box([left, right]).unwrap();
        let t = transform(canvas, left, 32.0 / 9.0);
        // A 32:9 source already covers the 3840x1080 canvas exactly: no zoom,
        // but the left screen still has to pan to its half.
        assert!((t.zoom - 0.0).abs() < 1e-9);
        assert!((t.pan_x - 0.25).abs() < 1e-9);
        assert_eq!(t.pan_y, 0.0);
    }

    #[test]
    fn degenerate_values_are_ignored() {
        let rect = Rect::new(0, 0, 1920, 1080);
        assert_eq!(transform(rect, rect, 0.0), Transform::default());
        assert_eq!(transform(rect, rect, f64::NAN), Transform::default());
        assert!(bounding_box([]).is_none());
    }
}
