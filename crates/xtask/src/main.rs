use std::{
    env, fs, path::PathBuf, process::{exit, Command}
};


#[derive(Debug)]
struct Target {
    name: &'static str,
    triple: &'static str,
    dir: &'static str,
    exe_suffix: &'static str,
}

const TARGETS: &[Target] = &[
    Target {
        name: "linux",
        triple: "x86_64-unknown-linux-musl",
        dir: "linux",
        exe_suffix: "",
    },
    Target {
        name: "macos_intel",
        triple: "x86_64-apple-darwin",
        dir: "macos_intel",
        exe_suffix: "",
    },
    Target {
        name: "macos_arm",
        triple: "aarch64-apple-darwin",
        dir: "macos_arm",
        exe_suffix: "",
    },
    Target {
        name: "windows",
        triple: "x86_64-pc-windows-gnu",
        dir: "windows",
        exe_suffix: ".exe",
    },
];

fn detect_host_target() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "x86_64") {
            "macos_intel"
        } else {
            "macos_arm"
        }
    } else {
        "linux"
    }
}

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
    let channels_out_dir = PathBuf::from("channels");
    let stop_dir = PathBuf::from("greentic/plugins/channels/stopped"); // or wherever you want to copy
    
    // Ensure base output dirs exist
    for target in TARGETS {
        fs::create_dir_all(channels_out_dir.join(target.dir)).unwrap();
    }
    
    // make sure the output directory exists
    if let Err(e) = fs::create_dir_all(&stop_dir) {
        eprintln!("Failed to create {}: {}", stop_dir.display(), e);
        exit(1);
    }

    
    for pkg in &selected_channels {
        for target in TARGETS {
            println!("🔨 Building `{}` for target `{}`…", pkg, target.triple);
            let status = Command::new("cargo")
                .args([
                    "build",
                    "--release",
                    "--target",
                    target.triple,
                    "--package",
                    pkg,
                    "--bin",
                    pkg,
                ])
                .status()
                .unwrap_or_else(|e| {
                    eprintln!("Failed to launch cargo for `{}`: {}", pkg, e);
                    exit(1);
                });
            if !status.success() {
                eprintln!("Cargo build failed for `{}` target `{}`.", pkg, target.triple);
                exit(1);
            }

            let exe_name = format!("{pkg}{}", target.exe_suffix);
            let built_path = crate_dir
                .join("target")
                .join(target.triple)
                .join("release")
                .join(&exe_name);
            let target_dir = channels_out_dir.join(target.dir);
            let dest_path = target_dir.join(&exe_name);

            if let Err(e) = fs::copy(&built_path, &dest_path) {
                eprintln!(
                    "❌ Failed to copy `{}` → `{}`: {}",
                    built_path.display(),
                    dest_path.display(),
                    e
                );
                exit(1);
            }

            println!("✅ Copied to {}", dest_path.display());
        }
    }

    // 🧠 Detect host and copy correct binary to stopped/
    let host_target = detect_host_target();

    for pkg in &selected_channels {
        let target = TARGETS
            .iter()
            .find(|t| t.name == host_target)
            .expect("Missing target");
        let exe_name = format!("{pkg}{}", target.exe_suffix);
        let src = channels_out_dir.join(target.dir).join(&exe_name);
        let dst = stop_dir.join(&exe_name);

        if let Err(e) = fs::copy(&src, &dst) {
            eprintln!(
                "❌ Failed to copy host binary `{}` → `{}`: {}",
                src.display(),
                dst.display(),
                e
            );
            exit(1);
        }
        println!("🚚 Host binary copied to {}", dst.display());
    }

    println!("\n🎉 All plugins built and placed into `channels/*` and `greentic/plugins/channels/stopped`.");

}
