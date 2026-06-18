use std::env;

pub struct Args {
    pub video_path: String,
}

pub fn parse() -> Args {
    let args: Vec<String> = env::args().collect();

    let mut video_path: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                eprintln!("Usage: {} [OPTIONS] <path-to-video>", args[0]);
                eprintln!();
                eprintln!("Options:");
                eprintln!("  -h, --help                      Show this help");
                eprintln!();
                eprintln!("Example: {} path/to/wallpaper.mp4", args[0]);
                std::process::exit(0);
            }
            _ => {
                if video_path.is_none() {
                    video_path = Some(args[i].clone());
                } else {
                    eprintln!("Too many arguments");
                    std::process::exit(0);
                }
            }
        }
        i += 1;
    }

    let video_path = match video_path {
        Some(p) => p,
        None => {
            eprintln!("Usage: {} [OPTIONS] <path-to-video>", args[0]);
            eprintln!("Example: {} path/to/wallpaper.mp4", args[0]);
            std::process::exit(1);
        }
    };

    Args { video_path }
}
