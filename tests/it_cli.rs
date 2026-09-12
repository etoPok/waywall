use waywall::cli::args::parse_from;

#[test]
fn integration_cli_via_lib() {
    let args = parse_from(
        ["waywall", "-o", "eDP-1,DP-3", "--vaapi", "video.mp4"]
            .into_iter()
            .map(|s| s.to_string()),
    )
    .unwrap();

    assert_eq!(args.video_path, "video.mp4");
    assert_eq!(args.outputs, vec!["eDP-1", "DP-3"]);
    assert!(args.use_vaapi);
    assert!(args.use_egl_gl);
}
