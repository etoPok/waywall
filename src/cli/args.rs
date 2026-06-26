use std::env;

pub struct Args {
    pub video_path: String,
    pub outputs: Vec<String>,
}

fn print_usage(program: &str) {
    eprintln!("Usage: {program} [OPTIONS] <path-to-video>");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  -o, --output <name>  Output connector(s) to use (e.g. eDP-1, DP-3)");
    eprintln!("                       Can be specified multiple times or comma-separated");
    eprintln!("  -h, --help           Show this help");
    eprintln!();
    eprintln!("Example: {program} path/to/wallpaper.mp4");
    eprintln!("         {program} -o eDP-1 path/to/wallpaper.mp4");
    eprintln!("         {program} -o eDP-1 -o DP-3 path/to/wallpaper.mp4");
}

pub fn parse() -> Args {
    let args: Vec<String> = env::args().collect();

    let mut video_path: Option<String> = None;
    let mut outputs: Vec<String> = Vec::new();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                print_usage(&args[0]);
                std::process::exit(0);
            }
            "--output" | "-o" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --output/-o requires a value");
                    std::process::exit(1);
                }
                for name in args[i].split(',') {
                    let trimmed = name.trim();
                    if !trimmed.is_empty() {
                        outputs.push(trimmed.to_string());
                    }
                }
            }
            _ => {
                if video_path.is_none() {
                    video_path = Some(args[i].clone());
                } else {
                    eprintln!("error: unexpected argument '{}'", args[i]);
                    print_usage(&args[0]);
                    std::process::exit(1);
                }
            }
        }
        i += 1;
    }

    let video_path = match video_path {
        Some(p) => p,
        None => {
            print_usage(&args[0]);
            std::process::exit(1);
        }
    };

    Args { video_path, outputs }
}
