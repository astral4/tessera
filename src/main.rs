#![feature(array_chunks)]

use anyhow::{Result, bail};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer, images::Image};
use foldhash::{HashMap, HashMapExt};
use image::{ImageReader, RgbaImage};
use kiddo::{ImmutableKdTree, SquaredEuclidean};
use pico_args::Arguments;
use std::{fmt::Display, path::PathBuf, str::FromStr};
use walkdir::WalkDir;

const PALETTE_IMAGE_WIDTH: u32 = 16;
const PALETTE_IMAGE_HEIGHT: u32 = 16;
const RGBA8_PIXEL_SIZE: usize = 4;
const PALETTE_SCALE: f32 = (u8::MAX as u32 * PALETTE_IMAGE_WIDTH * PALETTE_IMAGE_HEIGHT) as f32;

fn parse_arg<T>(args: &mut Arguments, (short, long): (&'static str, &'static str)) -> Result<T>
where
    T: FromStr,
    <T as FromStr>::Err: Display,
{
    match (
        args.opt_value_from_str(short)?,
        args.opt_value_from_str(long)?,
    ) {
        (Some(arg), None) | (None, Some(arg)) => Ok(arg),
        (Some(_), Some(_)) => bail!(
            "duplicate argument specified; only one of `{short}` and `{long}` should be present"
        ),
        (None, None) => bail!("no `{short}` or `{long}` argument specified"),
    }
}

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
    let mut args = Arguments::from_env();

    let palette_dir_path: PathBuf = parse_arg(&mut args, ("-p", "--palette-dir"))?;
    let input_image_path: PathBuf = parse_arg(&mut args, ("-i", "--input"))?;
    let output_image_path: PathBuf = parse_arg(&mut args, ("-o", "--output"))?;

    let mut palette_colors = Vec::new();
    let mut palette_images = Vec::new();

    for entry in WalkDir::new(palette_dir_path) {
        let entry = entry?;

        if entry.file_type().is_dir() {
            continue;
        }

        let path = entry.path();

        if path.extension().is_none_or(|ext| {
            !matches!(ext.to_str(), Some("avif" | "jpeg" | "jpg" | "png" | "webp"))
        }) {
            continue;
        }

        let image = ImageReader::open(path)?.decode()?.into_rgba8();
        let mut resized_image = resize_image(image, PALETTE_IMAGE_WIDTH, PALETTE_IMAGE_HEIGHT)?;

        let (mut r_sum, mut g_sum, mut b_sum) = (0., 0., 0.);

        for px in resized_image
            .buffer_mut()
            .array_chunks_mut::<RGBA8_PIXEL_SIZE>()
        {
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
                px[3] = u8::MAX;

                r_sum += r;
                g_sum += g;
                b_sum += b;
            }
        }

        let oklab = linear_srgb_to_oklab(
            r_sum / PALETTE_SCALE,
            g_sum / PALETTE_SCALE,
            b_sum / PALETTE_SCALE,
        );

        palette_colors.push(oklab);
        palette_images.push(resized_image.into_vec());
    }

    let tree = ImmutableKdTree::new_from_slice(&palette_colors);

    let input_image = ImageReader::open(input_image_path)?.decode()?.into_rgba8();
    let (width, height) = input_image.dimensions();
    let resized_image = resize_image(input_image, width / 2, height / 2)?;

    let mut output_image = RgbaImage::new(
        resized_image.width() * PALETTE_IMAGE_WIDTH,
        resized_image.height() * PALETTE_IMAGE_HEIGHT,
    );

    let mut palette_cache =
        HashMap::with_capacity((resized_image.width() * resized_image.height() / 2) as usize);

    for (input_px, tile_idx) in resized_image
        .buffer()
        .array_chunks::<RGBA8_PIXEL_SIZE>()
        .zip(0..)
    {
        let palette_image = palette_cache.entry(input_px).or_insert_with(|| {
            const SCALE: f32 = 255. * 255.;

            let a = f32::from(input_px[3]);
            let r = f32::from(input_px[0]) * a / SCALE;
            let g = f32::from(input_px[1]) * a / SCALE;
            let b = f32::from(input_px[2]) * a / SCALE;
            let oklab = linear_srgb_to_oklab(r, g, b);
            let palette_idx = tree.nearest_one::<SquaredEuclidean>(&oklab).item as usize;
            palette_images.get(palette_idx).unwrap()
        });

        for (tile_px, px_idx) in palette_image.array_chunks::<RGBA8_PIXEL_SIZE>().zip(0..) {
            let tile_x = tile_idx % resized_image.width();
            let tile_y = tile_idx / resized_image.width();

            let px_x = px_idx % PALETTE_IMAGE_WIDTH;
            let px_y = px_idx / PALETTE_IMAGE_WIDTH;

            let x = tile_x * PALETTE_IMAGE_WIDTH + px_x;
            let y = tile_y * PALETTE_IMAGE_HEIGHT + px_y;

            output_image.put_pixel(x, y, image::Rgba(*tile_px));
        }
    }

    output_image.save(output_image_path)?;

    Ok(())
}
