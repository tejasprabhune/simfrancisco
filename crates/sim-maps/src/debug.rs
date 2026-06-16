/// Debug rendering: write a semantic grid as a colored PNG.
///
/// One pixel per cell. Scale with --scale to produce a larger image.

use std::path::Path;

use anyhow::{Context, Result};
use image::{RgbImage, Rgb};

use crate::types::{Grid, SemanticClass};

/// Color palette for the debug PNG (R, G, B).
pub fn semantic_color(class: SemanticClass) -> [u8; 3] {
    match class {
        SemanticClass::Grass         => [100, 180,  80],
        SemanticClass::ParkGrass     => [ 60, 160,  60],
        SemanticClass::Sand          => [220, 200, 150],
        SemanticClass::Path          => [180, 160, 140],
        SemanticClass::Sidewalk      => [210, 210, 210],
        SemanticClass::Road          => [100, 100, 100],
        SemanticClass::Stairs        => [150, 120,  90],
        SemanticClass::CliffFace     => [120,  90,  60],
        SemanticClass::BuildingFloor => [220, 200, 180],
        SemanticClass::BuildingWall  => [155, 130, 110],
        SemanticClass::Water         => [ 60, 130, 210],
        SemanticClass::Plaza         => [205, 190, 160],
        SemanticClass::Shoreline     => [ 80, 155, 150],
        SemanticClass::BuildingMid   => [190, 175, 155],
        SemanticClass::BuildingTall  => [140, 150, 160],
    }
}

/// Write the semantic grid to a PNG. Each cell becomes one pixel.
/// Optionally scale the image for easier inspection.
pub fn dump_semantic_png(
    grid: &Grid<SemanticClass>,
    path: &Path,
    scale: u32,
) -> Result<()> {
    let scale = scale.max(1);
    let pw = grid.width * scale;
    let ph = grid.height * scale;
    let mut img = RgbImage::new(pw, ph);

    for row in 0..grid.height {
        for col in 0..grid.width {
            let [r, g, b] = semantic_color(*grid.get(col, row));
            let px = Rgb([r, g, b]);
            for dy in 0..scale {
                for dx in 0..scale {
                    img.put_pixel(col * scale + dx, row * scale + dy, px);
                }
            }
        }
    }

    img.save(path)
        .with_context(|| format!("save debug PNG: {}", path.display()))
}

/// Write a collision grid (0 = walkable, 255 = blocked) as a grayscale PNG.
pub fn dump_collision_png(
    grid: &Grid<u8>,
    path: &Path,
    scale: u32,
) -> Result<()> {
    let scale = scale.max(1);
    let pw = grid.width * scale;
    let ph = grid.height * scale;
    let mut img = RgbImage::new(pw, ph);

    for row in 0..grid.height {
        for col in 0..grid.width {
            let v = *grid.get(col, row);
            let shade = if v == 255 { 30u8 } else { 200 - v.min(180) };
            let px = Rgb([shade, shade, shade]);
            for dy in 0..scale {
                for dx in 0..scale {
                    img.put_pixel(col * scale + dx, row * scale + dy, px);
                }
            }
        }
    }

    img.save(path)
        .with_context(|| format!("save collision PNG: {}", path.display()))
}
