use std::env;

#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub video_path: String,
    pub outputs: Vec<String>,
    pub use_vaapi: bool,
    pub use_gl: bool,
}

fn print_usage(program: &str) {
    eprintln!("Usage: {program} [OPTIONS] <path-to-video>");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  -o, --output <name>  Output connector(s) to use (e.g. eDP-1, DP-3)");
    eprintln!("                       Can be specified multiple times or comma-separated");
    eprintln!("  --vaapi              Enable hardware-accelerated decoding (VA-API)");
    eprintln!("  --no-gl              Disable OpenGL/EGL rendering (default is false)");
    eprintln!("  -h, --help           Show this help");
    eprintln!();
    eprintln!("Example: {program} path/to/wallpaper.mp4");
    eprintln!("         {program} -o eDP-1 path/to/wallpaper.mp4");
    eprintln!("         {program} -o eDP-1 -o DP-3 path/to/wallpaper.mp4");
}

pub fn parse_from<I>(args: I) -> Result<Args, String>
where
    I: IntoIterator<Item = String>,
{
    let args: Vec<String> = args.into_iter().collect();
    if args.is_empty() {
        return Err("missing program name".into());
    }

    let mut video_path: Option<String> = None;
    let mut outputs: Vec<String> = Vec::new();
    let mut use_vaapi: bool = false;
    let mut use_gl: bool = true;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                return Err("help".into());
            }
            "--output" | "-o" => {
                i += 1;
                if i >= args.len() {
                    return Err("error: --output/-o requires a value".into());
                }
                for name in args[i].split(',') {
                    let trimmed = name.trim();
                    if !trimmed.is_empty() {
                        outputs.push(trimmed.to_string());
                    }
                }
            }
            "--vaapi" => {
                use_vaapi = true;
            }
            "--no-gl" => {
                use_gl = false;
            }
            _ => {
                if video_path.is_none() {
                    video_path = Some(args[i].clone());
                } else {
                    return Err(format!("error: unexpected argument '{}'", args[i]));
                }
            }
        }
        i += 1;
    }

    let video_path = match video_path {
        Some(p) => p,
        None => return Err("error: missing video path".into()),
    };

    Ok(Args {
        video_path,
        outputs,
        use_vaapi,
        use_gl,
    })
}

pub fn parse() -> Args {
    let args: Vec<String> = env::args().collect();
    match parse_from(args.clone()) {
        Ok(a) => a,
        Err(e) if e == "help" => {
            print_usage(&args[0]);
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("{e}");
            print_usage(&args[0]);
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parse_minimal_video_only() {
        let a = parse_from(args(&["waywall", "video.mp4"])).unwrap();
        assert_eq!(a.video_path, "video.mp4");
        assert!(a.outputs.is_empty());
        assert!(!a.use_vaapi);
        assert!(a.use_gl);
    }

    #[test]
    fn parse_output_single_and_comma() {
        let a = parse_from(args(&["waywall", "-o", "eDP-1,DP-3", "vid.mp4"])).unwrap();
        assert_eq!(a.outputs, vec!["eDP-1", "DP-3"]);

        let b = parse_from(args(&["waywall", "-o", "eDP-1", "-o", "DP-3", "vid.mp4"])).unwrap();
        assert_eq!(b.outputs, vec!["eDP-1", "DP-3"]);

        let c = parse_from(args(&[
            "waywall",
            "--output",
            " eDP-1 , , DP-3 ",
            "vid.mp4",
        ]))
        .unwrap();
        assert_eq!(c.outputs, vec!["eDP-1", "DP-3"]);
    }

    #[test]
    fn parse_flags_vaapi_no_gl() {
        let a = parse_from(args(&["waywall", "--vaapi", "--no-gl", "v.mp4"])).unwrap();
        assert!(a.use_vaapi);
        assert!(!a.use_gl);
    }

    #[test]
    fn parse_missing_output_value_errors() {
        let e = parse_from(args(&["waywall", "-o"])).unwrap_err();
        assert!(e.contains("requires a value"));
    }

    #[test]
    fn parse_unexpected_extra_arg_errors() {
        let e = parse_from(args(&["waywall", "a.mp4", "b.mp4"])).unwrap_err();
        assert!(e.contains("unexpected argument"));
    }

    #[test]
    fn parse_missing_video_errors() {
        let e = parse_from(args(&["waywall", "-o", "eDP-1"])).unwrap_err();
        assert!(e.contains("missing video"));
    }

    #[test]
    fn parse_help_returns_err_help() {
        let e = parse_from(args(&["waywall", "--help", "v.mp4"])).unwrap_err();
        assert_eq!(e, "help");
        let e2 = parse_from(args(&["waywall", "-h"])).unwrap_err();
        assert_eq!(e2, "help");
    }
}
