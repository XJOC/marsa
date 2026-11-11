//! The icon's image, drawn in code so no image file ships: a ring with a dot,
//! the shape the S-09 spike used. A temporary, simple image until
//! `docs/design/` exists (AGENTS.md section 9).
//!
//! The shape is white with a thin dark rim, so it shows on a light and on a
//! dark bar (Windows, Linux). On macOS it is a template image: the system
//! reads only its alpha channel and recolours it for the menu bar.

/// The side of the image in pixels. Each bar scales it to its own size.
pub const SIZE: u32 = 32;

/// The RGBA bytes of the image, row by row: `SIZE * SIZE * 4` of them.
pub fn rgba() -> Vec<u8> {
    let size = SIZE as usize;
    let center = (SIZE as f32 - 1.0) / 2.0;
    let mut bytes = Vec::with_capacity(size * size * 4);
    for index in 0..size * size {
        let x = (index % size) as f32 - center;
        let y = (index / size) as f32 - center;
        bytes.extend_from_slice(&pixel((x * x + y * y).sqrt()));
    }
    bytes
}

/// The pixel at `distance` from the centre: the dot, the ring, the rim of
/// each, or nothing.
fn pixel(distance: f32) -> [u8; 4] {
    const FILL: [u8; 4] = [255, 255, 255, 255];
    const RIM: [u8; 4] = [0, 0, 0, 160];
    const NONE: [u8; 4] = [0, 0, 0, 0];
    if distance <= 5.0 || (11.0..=14.0).contains(&distance) {
        FILL
    } else if distance <= 6.0 || (10.0..=15.0).contains(&distance) {
        RIM
    } else {
        NONE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
        let start = ((y * SIZE + x) * 4) as usize;
        bytes[start..start + 4].try_into().unwrap()
    }

    #[test]
    fn the_image_has_one_rgba_pixel_per_point() {
        assert_eq!(rgba().len(), (SIZE * SIZE * 4) as usize);
    }

    #[test]
    fn the_dot_and_the_ring_are_drawn_and_the_corners_are_clear() {
        let bytes = rgba();
        let middle = SIZE / 2;
        assert_eq!(at(&bytes, middle, middle)[3], 255, "the dot");
        assert_eq!(
            at(&bytes, middle, 3)[3],
            255,
            "the ring, 12.5 from the centre"
        );
        assert_eq!(at(&bytes, middle, 8)[3], 0, "between the dot and the ring");
        assert_eq!(at(&bytes, 0, 0)[3], 0, "the corner");
        assert_eq!(at(&bytes, SIZE - 1, SIZE - 1)[3], 0, "the corner");
    }

    #[test]
    fn the_image_is_symmetric() {
        let bytes = rgba();
        for y in 0..SIZE {
            for x in 0..SIZE {
                let pixel = at(&bytes, x, y);
                assert_eq!(
                    pixel,
                    at(&bytes, SIZE - 1 - x, y),
                    "({x}, {y}) left to right"
                );
                assert_eq!(
                    pixel,
                    at(&bytes, x, SIZE - 1 - y),
                    "({x}, {y}) top to bottom"
                );
            }
        }
    }
}
