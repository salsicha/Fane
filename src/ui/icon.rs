//! Fane's icon (a light-blue four-pointed star on a dark disc), drawn at any size.
//! Mirrors `assets/fane.svg`, so no image files or SVG renderer are needed at runtime.

use image::{Rgba, RgbaImage};
use imageproc::drawing::{draw_filled_circle_mut, draw_polygon_mut};
use imageproc::point::Point;

pub fn rgba(size: u32) -> RgbaImage {
    // Supersample 4x, then downscale for smooth edges.
    let s = size * 4;
    let k = s as f32 / 64.0;
    let mut img = RgbaImage::from_pixel(s, s, Rgba([0, 0, 0, 0]));
    let c = (32.0 * k) as i32;
    draw_filled_circle_mut(&mut img, (c, c), (30.0 * k) as i32, Rgba([0x20, 0x21, 0x26, 255]));
    let pts = [
        (32.0, 12.0), (36.5, 27.5), (52.0, 32.0), (36.5, 36.5),
        (32.0, 52.0), (27.5, 36.5), (12.0, 32.0), (27.5, 27.5),
    ];
    let poly: Vec<Point<i32>> = pts.iter().map(|(x, y)| Point::new((x * k) as i32, (y * k) as i32)).collect();
    draw_polygon_mut(&mut img, &poly, Rgba([0x8a, 0xb4, 0xf8, 255]));
    image::imageops::resize(&img, size, size, image::imageops::FilterType::Triangle)
}

#[cfg(test)]
mod tests {
    #[test]
    fn icon_has_star_and_transparent_corners() {
        let img = super::rgba(64);
        assert_eq!(img.get_pixel(0, 0).0[3], 0);
        let center = img.get_pixel(32, 32).0;
        assert!(center[2] > 200 && center[3] == 255, "{center:?}");
    }
}
