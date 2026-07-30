//! Desktop entry point: Windows, macOS and Linux.

fn main() {
    // `warn` by default keeps wgpu's per-frame chatter out of the way; raise it
    // with e.g. `RUST_LOG=umber_app=debug,wgpu_core=info`.
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,umber_app=info,umber_render=info"),
    )
    .init();

    if let Err(e) = umber_app::run() {
        log::error!("umber exited with an error: {e}");
        std::process::exit(1);
    }
}
