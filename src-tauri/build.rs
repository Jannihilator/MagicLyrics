fn main() {
    // Declaring the app's commands lets the capability file allow exactly these and nothing else.
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "get_state",
            "get_settings",
            "update_settings",
            "toggle_play_pause",
            "set_controls_open",
            "set_hit_rects",
            "refetch_lyrics",
            "set_lyrics_offset",
            "quit",
        ]),
    ))
    .expect("failed to run tauri-build");
}
