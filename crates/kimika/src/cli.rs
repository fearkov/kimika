//! the command line.

pub const USAGE: &str = "\
usage, kimika <game folder, .vpk or eboot.bin> [--scale <n>] [--headless <frames>]

--scale opens the window at n times the console's 960x544. --headless runs
the game with no window for so many frames and says how it went.";

#[derive(Debug, Default)]
pub struct Options {
    pub game: Option<String>,
    pub scale: Option<u32>,
    pub headless: Option<u64>,
}

pub fn parse() -> Result<Options, String> {
    parse_from(std::env::args().skip(1))
}

fn parse_from(args: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut options = Options::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut number = |name: &str| -> Result<u64, String> {
            let value = args.next().ok_or_else(|| format!("{name} needs a number\n{USAGE}"))?;
            value.parse().map_err(|_| format!("{name} needs a number, not {value}\n{USAGE}"))
        };
        match arg.as_str() {
            "--scale" => options.scale = Some(number("--scale")?.clamp(1, 8) as u32),
            "--headless" => options.headless = Some(number("--headless")?),
            "--help" | "-h" => return Err(USAGE.to_owned()),
            other if other.starts_with("--") => return Err(format!("unknown option {other}\n{USAGE}")),
            _ if options.game.is_none() => options.game = Some(arg),
            _ => return Err(format!("one game at a time, {arg} is one too many\n{USAGE}")),
        }
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options, String> {
        parse_from(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn a_game_and_its_options_come_in_any_order() {
        let options = parse(&["--scale", "2", "hello.vpk", "--headless", "60"]).unwrap();
        assert_eq!(options.game.as_deref(), Some("hello.vpk"));
        assert_eq!((options.scale, options.headless), (Some(2), Some(60)));
        assert!(parse(&["--scale"]).is_err());
        assert!(parse(&["--fast", "a.vpk"]).is_err());
        assert!(parse(&["a.vpk", "b.vpk"]).is_err());
    }
}
