use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use image::{DynamicImage, ImageFormat, RgbaImage};
use std::{
    env, fs,
    io::Cursor,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Parser)]
#[command(about = "Build a local, ad-hoc-signed Nightjar macOS app bundle")]
struct Args {
    #[command(subcommand)]
    command: Task,
}

#[derive(Subcommand)]
enum Task {
    /// Build Nightjar and its Rust sidecars, then create a NEW .app bundle.
    BundleMacos {
        /// New bundle path. Existing bundles are never overwritten.
        #[arg(long, default_value = "dist/Nightjar.app")]
        output: PathBuf,
        /// Use debug builds for a faster development cycle.
        #[arg(long)]
        debug: bool,
        /// Skip local ad-hoc signing (not recommended for opening the app).
        #[arg(long)]
        skip_sign: bool,
    },
}

fn main() -> Result<()> {
    let Args { command } = Args::parse();
    match command {
        Task::BundleMacos {
            output,
            debug,
            skip_sign,
        } => bundle(&output, debug, skip_sign),
    }
}

fn checked(command: &mut Command, stage: &str) -> Result<()> {
    ensure!(
        command
            .status()
            .with_context(|| format!("Start {stage}"))?
            .success(),
        "{stage} failed"
    );
    Ok(())
}

fn bundle(output: &Path, debug: bool, skip_sign: bool) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("Build the .app on macOS with the native Rust toolchain and macOS SDK");
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("Workspace root")?;
    let output = if output.is_absolute() {
        output.to_owned()
    } else {
        root.join(output)
    };
    ensure!(
        output.extension().is_some_and(|ext| ext == "app"),
        "Output must end in .app"
    );
    ensure!(
        fs::symlink_metadata(&output).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
        "Bundle already exists or is inaccessible: {}. Choose a new --output path",
        output.display()
    );
    let mut build = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    build.current_dir(root).args([
        "build",
        "--locked",
        "-p",
        "nightjar",
        "-p",
        "fieldkit",
        "-p",
        "fieldkit-agent",
        "--bins",
    ]);
    if !debug {
        build.arg("--release");
    }
    checked(&mut build, "Rust app and sidecar build")?;
    let target = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    let target = if target.is_absolute() {
        target
    } else {
        root.join(target)
    };
    let binaries = target.join(if debug { "debug" } else { "release" });
    for name in ["nightjar", "fieldkit", "fieldkit-agent"] {
        ensure!(
            binaries.join(name).is_file(),
            "Missing native binary {name}"
        );
    }
    fs::create_dir_all(output.parent().context("Bundle parent")?)?;
    // Exclusive creation: never erase or replace an existing application.
    fs::create_dir(&output)?;
    let contents = output.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    fs::create_dir_all(&macos)?;
    fs::create_dir_all(&resources)?;
    for name in ["nightjar", "fieldkit", "fieldkit-agent"] {
        fs::copy(binaries.join(name), macos.join(name))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(macos.join(name), fs::Permissions::from_mode(0o755))?;
        }
    }
    fs::write(contents.join("Info.plist"), info_plist())?;
    fs::write(contents.join("PkgInfo"), b"APPL????")?;
    fs::write(resources.join("Nightjar.icns"), icon()?)?;
    if !skip_sign {
        for name in ["fieldkit", "fieldkit-agent"] {
            checked(
                Command::new("/usr/bin/codesign")
                    .args(["--force", "--sign", "-"])
                    .arg(macos.join(name)),
                "sidecar ad-hoc signing",
            )?;
        }
        checked(
            Command::new("/usr/bin/codesign")
                .args(["--force", "--sign", "-"])
                .arg(&output),
            "app ad-hoc signing",
        )?;
        checked(
            Command::new("/usr/bin/codesign")
                .args(["--verify", "--deep", "--strict"])
                .arg(&output),
            "app signature verification",
        )?;
    }
    println!("{}", output.display());
    println!("Local development bundle only; not Developer ID signed or notarized.");
    Ok(())
}

fn info_plist() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>Nightjar</string>
<key>CFBundleDisplayName</key><string>Nightjar</string>
<key>CFBundleIdentifier</key><string>dev.nightjar.app</string>
<key>CFBundleExecutable</key><string>nightjar</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>CFBundleVersion</key><string>1</string>
<key>CFBundleIconFile</key><string>Nightjar.icns</string>
<key>LSMinimumSystemVersion</key><string>12.0</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict></plist>
"#
}

fn icon_png(size: u32) -> Result<Vec<u8>> {
    let icon = nightjar_brand::app_icon(size);
    let image = RgbaImage::from_raw(size, size, icon.rgba).context("Icon pixel dimensions")?;
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image).write_to(&mut output, ImageFormat::Png)?;
    Ok(output.into_inner())
}

fn icon() -> Result<Vec<u8>> {
    let mut entries = Vec::new();
    for (tag, size) in [
        (b"icp4", 16),
        (b"icp5", 32),
        (b"icp6", 64),
        (b"ic07", 128),
        (b"ic08", 256),
        (b"ic09", 512),
        (b"ic10", 1024),
    ] {
        let png = icon_png(size)?;
        entries.extend_from_slice(tag);
        entries.extend_from_slice(&u32::try_from(png.len() + 8)?.to_be_bytes());
        entries.extend_from_slice(&png);
    }
    let mut result = b"icns".to_vec();
    result.extend_from_slice(&u32::try_from(entries.len() + 8)?.to_be_bytes());
    result.extend(entries);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_has_valid_icns_lengths_and_decodable_pngs() {
        let bytes = icon().unwrap();
        assert_eq!(&bytes[..4], b"icns");
        assert_eq!(
            u32::from_be_bytes(bytes[4..8].try_into().unwrap()) as usize,
            bytes.len()
        );
        let mut offset = 8;
        for (tag, size) in [
            (b"icp4", 16),
            (b"icp5", 32),
            (b"icp6", 64),
            (b"ic07", 128),
            (b"ic08", 256),
            (b"ic09", 512),
            (b"ic10", 1024),
        ] {
            assert_eq!(&bytes[offset..offset + 4], tag);
            let length =
                u32::from_be_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
            let image = image::load_from_memory(&bytes[offset + 8..offset + length]).unwrap();
            assert_eq!((image.width(), image.height()), (size, size));
            offset += length;
        }
        assert_eq!(offset, bytes.len());
    }

    #[test]
    fn bundle_metadata_uses_nightjar_and_no_network_or_model_configuration() {
        let plist = info_plist();
        assert!(plist.contains("<string>nightjar</string>"));
        assert!(plist.contains("<string>Nightjar.icns</string>"));
        assert!(!plist.contains("API_KEY"));
        assert!(!plist.contains("NSAppTransportSecurity"));
    }
}
