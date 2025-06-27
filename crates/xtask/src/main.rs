use std::{
    env, fs, path::PathBuf, process::{exit, Command}
};

fn main() {
    // 1) List your channel crate names here:
    let all_channels = [
        "channel_telegram",
        "channel_mock_inout",
        "channel_mock_middle",
        "channel_ws",
        "channel_tester",
        // add more as needed…
    ];

    let args: Vec<String> = env::args().collect();

    // Optional filtering via CLI: cargo run -- channel_ws
    let selected_channels: Vec<&str> = if args.len() > 1 {
        let wanted: Vec<&str> = args[1..].iter().map(String::as_str).collect();
        for ch in &wanted {
            if !all_channels.contains(ch) {
                eprintln!("Unknown channel `{}`", ch);
                exit(1);
            }
        }
        wanted
    } else {
        all_channels.to_vec()
    };

    // Path to the `channel_telegram` crate
    let crate_dir = PathBuf::from(".");
    let out_dir = PathBuf::from("greentic/plugins/channels/stopped"); // or wherever you want to copy
    // make sure the output directory exists
    if let Err(e) = fs::create_dir_all(&out_dir) {
        eprintln!("Failed to create {}: {}", out_dir.display(), e);
        exit(1);
    }

    for pkg in &selected_channels {
        println!("Building `{}`…", pkg);

        // 3) Run `cargo build --release --package {pkg}`
        let status = Command::new("cargo")
            .args(["build", "--release", "-p", pkg, "--bin", pkg])
            .status()
            .unwrap_or_else(|e| {
                eprintln!("Failed to launch cargo for `{}`: {}", pkg, e);
                exit(1);
            });
        if !status.success() {
            eprintln!("Cargo build failed for `{}`.", pkg);
            exit(1);
        }

        // 4) 
        let exe_suffix = if cfg!(windows) { ".exe" } else { "" };
        let exe_name   = format!("{pkg}{exe_suffix}");

        // 5) Copy from target/release to plugins/channels
        let built_path = crate_dir
            .join("target")
            .join("release")
            .join(&exe_name);
        let dest_path = out_dir.join(&exe_name);

        if let Err(e) = fs::copy(&built_path, &dest_path) {
            eprintln!(
                "Failed to copy `{}` → `{}`: {}",
                built_path.display(),
                dest_path.display(),
                e
            );
            exit(1);
        }
        println!("Copied {} → {}", built_path.display(), dest_path.display());
    }
}
