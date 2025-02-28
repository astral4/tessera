use anyhow::{Result, bail};
use fast_image_resize::{PixelType, ResizeOptions, Resizer, images::Image};
use image::ImageReader;
use pico_args::Arguments;
use std::{fmt::Display, path::PathBuf, str::FromStr};
use walkdir::WalkDir;

const PALETTE_IMAGE_WIDTH: u32 = 16;
const PALETTE_IMAGE_HEIGHT: u32 = 16;

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

    let palette_dir: PathBuf = parse_arg(&mut args, ("-p", "--palette-dir"))?;

    for entry in WalkDir::new(palette_dir) {
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
        let (input_width, input_height) = image.dimensions();

        let image =
            Image::from_vec_u8(input_width, input_height, image.into_vec(), PixelType::U8x4)?;

        let mut resized_image =
            Image::new(PALETTE_IMAGE_WIDTH, PALETTE_IMAGE_HEIGHT, PixelType::U8x4);

        Resizer::new().resize(&image, &mut resized_image, &ResizeOptions::default())?;

        let (mut r, mut g, mut b) = (0, 0, 0);

        for px in resized_image.buffer().chunks_exact(4) {
            let a = u64::from(px[3]);
            r += u64::from(px[0]) * a;
            g += u64::from(px[1]) * a;
            b += u64::from(px[2]) * a;
        }

        let scale = 255. * 255. * (PALETTE_IMAGE_WIDTH * PALETTE_IMAGE_HEIGHT) as f32;

        let (r, g, b) = (r as f32 / scale, g as f32 / scale, b as f32 / scale);

        let oklab = linear_srgb_to_oklab(r, g, b);
    }

    Ok(())
}
