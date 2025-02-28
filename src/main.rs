use anyhow::{Result, bail};
use pico_args::Arguments;
use std::{fmt::Display, path::PathBuf, str::FromStr};

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

fn main() -> Result<()> {
    let mut args = Arguments::from_env();

    let palette_dir: PathBuf = parse_arg(&mut args, ("-p", "--palette-dir"))?;

    Ok(())
}
