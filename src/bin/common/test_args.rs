use std::env;

#[derive(Debug)]
pub struct TestArgs {
    pub prod: waywall::cli::args::Args,
    pub frames: u64,
}

fn print_usage(program: &str) {
    eprintln!("Usage: {program} [OPTIONS] <path-to-video>");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  -o, --output <name>        Output connector(s) to use (e.g. eDP-1, DP-3)");
    eprintln!("  --hwdec <no|vaapi>         Enable hardware-accelerated decoding (VA-API)");
    eprintln!("  -h, --help                 Show this help");
    eprintln!();
    eprintln!("Example: {program} path/to/wallpaper.mp4");
    eprintln!("         {program} -o eDP-1 path/to/wallpaper.mp4");
    eprintln!("         {program} -o eDP-1 -o DP-3 path/to/wallpaper.mp4");
}

pub fn parse_from<I>(args: I) -> Result<TestArgs, String>
where
    I: IntoIterator<Item = String>,
{
    let args: Vec<String> = args.into_iter().collect();
    if args.is_empty() {
        return Err("missing program name".into());
    }

    let mut frames: u64 = 0;
    let mut rest: Vec<String> = vec![args[0].clone()];

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--frames" => {
                i += 1;
                if i >= args.len() {
                    return Err("error: --frames requires a value".into());
                }
                match args[i].parse::<u64>() {
                    Ok(0) => return Err("frames must be >= 1".into()),
                    Ok(n) => frames = n,
                    Err(_) => {
                        return Err(format!("error: invalid --frames '{}'", args[i]));
                    }
                }
            }
            _ => {
                rest.push(args[i].clone());
            }
        }
        i += 1;
    }

    let prod = waywall::cli::args::parse_from(rest)?;

    Ok(TestArgs { prod, frames })
}

pub fn parse() -> TestArgs {
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
    fn defaults_are_hwdec_and_0_frames() {
        let a = parse_from(args(&["waywall-drm-test", "video.mp4"])).unwrap();
        assert_eq!(a.frames, 0);
        assert!(a.prod.use_hwdec);
        assert_eq!(a.prod.video_path, "video.mp4");
    }

    #[test]
    fn hwdec_vaapi_sets_flag() {
        let a = parse_from(args(&["waywall-drm-test", "--hwdec", "vaapi", "v.mp4"])).unwrap();
        assert!(a.prod.use_hwdec);
    }

    #[test]
    fn hwdec_no_clears_flag() {
        let a = parse_from(args(&[
            "waywall-drm-test",
            "--hwdec",
            "vaapi",
            "--hwdec",
            "no",
            "v.mp4",
        ]))
        .unwrap();
        assert!(!a.prod.use_hwdec);
    }

    #[test]
    fn hwdec_invalid_errors() {
        let e = parse_from(args(&["waywall-drm-test", "--hwdec", "vulkan", "v.mp4"])).unwrap_err();
        assert!(e.contains("invalid --hwdec"), "unexpected: {e}");
    }

    #[test]
    fn hwdec_missing_value_errors() {
        let e = parse_from(args(&["waywall-drm-test", "--hwdec"])).unwrap_err();
        assert!(e.contains("requires a value"), "unexpected: {e}");
    }

    #[test]
    fn frames_custom_value() {
        let a = parse_from(args(&["waywall-drm-test", "--frames", "120", "v.mp4"])).unwrap();
        assert_eq!(a.frames, 120);
    }

    #[test]
    fn frames_zero_errors() {
        let e = parse_from(args(&["waywall-drm-test", "--frames", "0", "v.mp4"])).unwrap_err();
        assert_eq!(e, "frames must be >= 1");
    }

    #[test]
    fn frames_invalid_errors() {
        let e = parse_from(args(&["waywall-drm-test", "--frames", "abc", "v.mp4"])).unwrap_err();
        assert!(e.contains("invalid --frames"), "unexpected: {e}");
    }

    #[test]
    fn frames_missing_value_errors() {
        let e = parse_from(args(&["waywall-drm-test", "--frames"])).unwrap_err();
        assert!(e.contains("requires a value"), "unexpected: {e}");
    }

    #[test]
    fn prod_flags_still_delegate() {
        let a = parse_from(args(&[
            "waywall-drm-test",
            "-o",
            "eDP-1,DP-3",
            "--hwdec",
            "vaapi",
            "--frames",
            "10",
            "video.mp4",
        ]))
        .unwrap();
        assert_eq!(a.prod.outputs, vec!["eDP-1", "DP-3"]);
        assert!(a.prod.use_hwdec);
        assert_eq!(a.frames, 10);
    }

    #[test]
    fn help_propagates_from_prod() {
        let e = parse_from(args(&["waywall-drm-test", "--help"])).unwrap_err();
        assert_eq!(e, "help");
    }

    #[test]
    fn missing_program_name_errors() {
        let e = parse_from(Vec::<String>::new()).unwrap_err();
        assert_eq!(e, "missing program name");
    }
}
