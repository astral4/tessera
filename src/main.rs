#![feature(array_chunks)]

use anyhow::{Result, bail};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer, images::Image};
use foldhash::{HashMap, HashMapExt};
use image::{GenericImageView, ImageReader, Pixel, Rgb, RgbImage, RgbaImage};
use kiddo::{ImmutableKdTree, SquaredEuclidean};
use pico_args::Arguments;
use std::path::PathBuf;
use walkdir::WalkDir;

type InputImage = RgbaImage;

const PIXEL_SIZE: usize = size_of::<<InputImage as GenericImageView>::Pixel>();

// Resizes the input image to the specified dimensions via triangle/bilinear sampling, producing a new image as output.
fn resize_image(image: RgbaImage, new_width: u32, new_height: u32) -> Result<Image<'static>> {
    let (width, height) = image.dimensions();
    let image = Image::from_vec_u8(width, height, image.into_vec(), PixelType::U8x4)?;
    let mut resized_image = Image::new(new_width, new_height, PixelType::U8x4);

    Resizer::new().resize(
        &image,
        &mut resized_image,
        &ResizeOptions::default().resize_alg(ResizeAlg::Interpolation(FilterType::Bilinear)),
    )?;

    Ok(resized_image)
}

// Converts a (R, B, G) triple in linear sRGB space (i.e. every component's value is from 0.0 to 1.0)
// to its corresponding (L, a, b) triple in Oklab space.
// From https://bottosson.github.io/posts/oklab/
fn linear_srgb_to_oklab(r: f32, g: f32, b: f32) -> [f32; 3] {
    let lp = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let mp = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let sp = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();

    let l = 0.2104542553 * lp + 0.7936177850 * mp - 0.0040720468 * sp;
    let a = 1.9779984951 * lp - 2.4285922050 * mp + 0.4505937099 * sp;
    let b = 0.0259040371 * lp + 0.7827717662 * mp - 0.8086757660 * sp;

    [l, a, b]
}

fn main() -> Result<()> {
    // Parse and validate input arguments
    let mut args = Arguments::from_env();

    if args.contains(["-h", "--help"]) {
        println!(
            "tessera: image mosaic generator
-h, --help           print this message
-p, --palette-dir    path to directory containing images to tile the output image with
-s, --tile-size      width and height of each tile in the output image, in pixels
-i, --input          input image path; input will be read from this location
-o, --output         output image path; output will be written to this location"
        );
        return Ok(());
    }

    let palette_dir_path: PathBuf = args.value_from_str(["-p", "--palette-dir"])?;
    let tile_size: u32 = args.value_from_str(["-s", "--tile-size"])?;
    let input_image_path: PathBuf = args.value_from_str(["-i", "--input"])?;
    let output_image_path: PathBuf = args.value_from_str(["-o", "--output"])?;

    if !palette_dir_path.is_dir() {
        bail!("`-p`/`--palette-dir`: path does not point to a directory");
    }
    if tile_size == 0 {
        bail!("`-s`/`--tile-size`: tile size cannot be zero");
    }
    if !input_image_path.is_file() {
        bail!("`-i`/`--input`: path does not point to a file");
    }

    // Calculate scaling factor used in computing the average color of a tile
    let palette_scale =
        const { <<InputImage as GenericImageView>::Pixel as Pixel>::Subpixel::MAX as f32 }
            * tile_size as f32
            * tile_size as f32;

    // Calculate average color of each tile in the palette
    let mut palette_colors = Vec::new();
    let mut palette_images = Vec::new();

    for entry in WalkDir::new(palette_dir_path) {
        // Only process images in supported formats
        let path = entry?.into_path();
        if path.is_dir()
            || path.extension().is_none_or(|ext| {
                !matches!(ext.to_str(), Some("avif" | "jpeg" | "jpg" | "png" | "webp"))
            })
        {
            continue;
        }

        let image = ImageReader::open(path)?.decode()?.into_rgba8();
        let mut resized_image = resize_image(image, tile_size, tile_size)?;

        let (mut r_sum, mut g_sum, mut b_sum) = (0., 0., 0.);

        for px in resized_image.buffer_mut().array_chunks_mut::<PIXEL_SIZE>() {
            // The output image is opaque. The average color calculation
            // assumes each pixel of the tile is over a black (r=0, g=0, b=0) background.
            // This also simplifies calculations for new RGB values when the source pixels of tiles are not opaque.
            if px[3] == u8::MAX {
                r_sum += f32::from(px[0]);
                g_sum += f32::from(px[1]);
                b_sum += f32::from(px[2]);
            } else {
                let a = f32::from(px[3]);

                let r = f32::from(px[0]) * a / 255.;
                let g = f32::from(px[1]) * a / 255.;
                let b = f32::from(px[2]) * a / 255.;

                px[0] = r as u8;
                px[1] = g as u8;
                px[2] = b as u8;

                r_sum += r;
                g_sum += g;
                b_sum += b;
            }
        }

        let oklab = linear_srgb_to_oklab(
            r_sum / palette_scale,
            g_sum / palette_scale,
            b_sum / palette_scale,
        );

        palette_colors.push(oklab);
        palette_images.push(resized_image.into_vec());
    }

    // Construct k-d tree for nearest-neighbor queries for colors
    let tree = ImmutableKdTree::new_from_slice(&palette_colors);

    let input_image = ImageReader::open(input_image_path)?.decode()?.into_rgba8();
    let (width, height) = input_image.dimensions();

    let mut output_image = RgbImage::new(width * tile_size, height * tile_size);

    // Cache nearest-neighbor queries to avoid repeating work
    // Heuristic for initial capacity: probably fewer than half of the pixels in the input image have unique colors.
    // Even if this ends up being incorrect, the capacity will simply double and will never double again.
    // (Except when `width` and `height` are odd numbers and every pixel in the input image is unique...)
    let mut palette_cache = HashMap::with_capacity((width * height / 2) as usize);

    for (input_px, tile_idx) in input_image.pixels().zip(0..) {
        // Get the tile with average color "nearest" to the color of the current pixel
        let palette_image = palette_cache.entry(input_px).or_insert_with(|| {
            // Calculate the Oklab value for a pixel over a black (r=0, g=0, b=0) background
            const SCALE: f32 = 255. * 255.;

            let a = f32::from(input_px[3]);
            let r = f32::from(input_px[0]) * a / SCALE;
            let g = f32::from(input_px[1]) * a / SCALE;
            let b = f32::from(input_px[2]) * a / SCALE;
            let oklab = linear_srgb_to_oklab(r, g, b);
            let palette_idx = tree.nearest_one::<SquaredEuclidean>(&oklab).item as usize;
            palette_images.get(palette_idx).unwrap()
        });

        // Place each pixel of the tile in the output image
        for (tile_px, px_idx) in palette_image.array_chunks::<PIXEL_SIZE>().zip(0..) {
            let tile_x = tile_idx % width;
            let tile_y = tile_idx / width;

            let px_x = px_idx % tile_size;
            let px_y = px_idx / tile_size;

            let x = tile_x * tile_size + px_x;
            let y = tile_y * tile_size + px_y;

            output_image.put_pixel(x, y, Rgb(*tile_px.first_chunk().unwrap()));
        }
    }

    output_image.save(output_image_path)?;

    Ok(())
}
