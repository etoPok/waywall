use std::env;

const FPS_MAX: u64 = 90;
const FPS_MIN: u64 = 15;

#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub video_path: String,
    pub outputs: Vec<String>,
    pub use_hwdec: bool,
    pub fps: Option<u64>,
}

fn print_usage(program: &str) {
    eprintln!("Usage: {program} [OPTIONS] <path-to-video>");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  -o, --output <name>        Output connector(s) to use (e.g. eDP-1, DP-3)");
    eprintln!(
        "  --fps <{FPS_MIN}-{FPS_MAX}>              Define the target frame rate for video playback"
    );
    eprintln!("  --hwdec <no|vaapi>         Enable hardware-accelerated decoding (VA-API)");
    eprintln!("  -h, --help                 Show this help");
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
    let mut use_hwdec: bool = false;
    let mut fps: Option<u64> = None;

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
            "--hwdec" => {
                i += 1;
                if i >= args.len() {
                    return Err("error: --backend requires a value (no|vaapi)".into());
                }
                match args[i].as_str() {
                    "no" => use_hwdec = false,
                    "vaapi" => use_hwdec = true,
                    other => {
                        return Err(format!(
                            "error: invalid --hwdec '{other}' (expected no|vaapi)"
                        ));
                    }
                }
            }
            "--fps" => {
                i += 1;
                if i >= args.len() {
                    return Err(format!(
                        "error: --fps requires a value (expected integer {FPS_MIN}-{FPS_MAX})",
                    ));
                }
                match args[i].parse::<u64>() {
                    Ok(n) => {
                        if !(FPS_MIN..=FPS_MAX).contains(&n) {
                            return Err(format!(
                                "error: --fps must be between {FPS_MIN} and {FPS_MAX} (got '{n}')"
                            ));
                        }
                        fps.replace(n);
                    }
                    Err(_) => {
                        return Err(format!(
                            "error: invalid --fps '{}' (expected integer {FPS_MIN}-{FPS_MAX})",
                            args[i]
                        ));
                    }
                }
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
        use_hwdec,
        fps,
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
        assert!(!a.use_hwdec);
        assert_eq!(a.fps, None);
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
        let a = parse_from(args(&["waywall", "--hwdec", "vaapi", "v.mp4"])).unwrap();
        assert!(a.use_hwdec);

        let b = parse_from(args(&["waywall", "--hwdec", "no", "v.mp4"])).unwrap();
        assert!(!b.use_hwdec);
    }

    #[test]
    fn parse_hwdec_invalid_errors() {
        let e = parse_from(args(&["waywall", "--hwdec", "bad", "v.mp4"])).unwrap_err();
        assert!(e.contains("invalid --hwdec"));
    }

    #[test]
    fn parse_hwdec_missing_value_errors() {
        let e = parse_from(args(&["waywall", "--hwdec"])).unwrap_err();
        assert!(e.contains("requires a value"));
        let e2 = parse_from(args(&["waywall", "--hwdec", "vaapi"])).unwrap_err();
        assert!(e2.contains("missing video"));
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

    #[test]
    fn parse_fps_valid_boundaries() {
        let min = FPS_MIN.to_string();
        let a = parse_from(args(&["waywall", "--fps", &min, "v.mp4"])).unwrap();
        assert_eq!(a.fps, Some(FPS_MIN));

        let b = parse_from(args(&["waywall", "--fps", "30", "v.mp4"])).unwrap();
        assert_eq!(b.fps, Some(30));

        let max = FPS_MAX.to_string();
        let c = parse_from(args(&["waywall", "--fps", &max, "v.mp4"])).unwrap();
        assert_eq!(c.fps, Some(FPS_MAX));
    }

    #[test]
    fn parse_fps_combined_with_flags() {
        let a = parse_from(args(&[
            "waywall", "--fps", "60", "-o", "eDP-1", "--hwdec", "vaapi", "v.mp4",
        ]))
        .unwrap();
        assert_eq!(a.fps, Some(60));
        assert_eq!(a.outputs, vec!["eDP-1"]);
        assert!(a.use_hwdec);
    }

    #[test]
    fn parse_fps_below_range_errors() {
        let low = (FPS_MIN - 1).to_string();
        let e = parse_from(args(&["waywall", "--fps", &low, "v.mp4"])).unwrap_err();
        assert!(e.contains(&format!("must be between {FPS_MIN} and {FPS_MAX}")));
        assert!(e.contains(&format!("(got '{low}')")));

        let e2 = parse_from(args(&["waywall", "--fps", "0", "v.mp4"])).unwrap_err();
        assert!(e2.contains(&format!("must be between {FPS_MIN} and {FPS_MAX}")));
        assert!(e2.contains("(got '0')"));
    }

    #[test]
    fn parse_fps_above_range_errors() {
        let high = (FPS_MAX + 1).to_string();
        let e = parse_from(args(&["waywall", "--fps", &high, "v.mp4"])).unwrap_err();
        assert!(e.contains(&format!("must be between {FPS_MIN} and {FPS_MAX}")));
        assert!(e.contains(&format!("(got '{high}')")));
    }

    #[test]
    fn parse_fps_non_numeric_errors() {
        let e = parse_from(args(&["waywall", "--fps", "abc", "v.mp4"])).unwrap_err();
        assert!(e.contains("invalid --fps"));
        assert!(e.contains(&format!("(expected integer {FPS_MIN}-{FPS_MAX})")));
    }

    #[test]
    fn parse_fps_negative_errors() {
        let e = parse_from(args(&["waywall", "--fps", "-5", "v.mp4"])).unwrap_err();
        assert!(e.contains("invalid --fps"));
    }

    #[test]
    fn parse_fps_float_errors() {
        let e = parse_from(args(&["waywall", "--fps", "30.5", "v.mp4"])).unwrap_err();
        assert!(e.contains("invalid --fps"));
    }

    #[test]
    fn parse_fps_overflow_errors() {
        let e =
            parse_from(args(&["waywall", "--fps", "99999999999999999999", "v.mp4"])).unwrap_err();
        assert!(e.contains("invalid --fps"));
    }

    #[test]
    fn parse_fps_missing_value_errors() {
        let e = parse_from(args(&["waywall", "--fps"])).unwrap_err();
        assert!(e.contains("requires a value"));
        assert!(e.contains(&format!("(expected integer {FPS_MIN}-{FPS_MAX})")));
    }

    #[test]
    fn parse_fps_last_wins() {
        let a = parse_from(args(&["waywall", "--fps", "30", "--fps", "60", "v.mp4"])).unwrap();
        assert_eq!(a.fps, Some(60));
    }
}
