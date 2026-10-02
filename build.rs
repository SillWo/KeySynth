use std::env;
use std::error::Error;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=resources/keysynth.rc");
    println!("cargo:rerun-if-changed=resources/keysynth.manifest");
    println!("cargo:rerun-if-changed=resources/keysynth.ico");
    println!("cargo:rerun-if-env-changed=RC");
    println!("cargo:rerun-if-env-changed=LLVM_WINDRES");
    println!("cargo:rerun-if-env-changed=WINDRES");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }

    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing CARGO_MANIFEST_DIR")?);
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").ok_or("missing OUT_DIR")?);
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let rc_file = manifest_dir.join("resources").join("keysynth.rc");

    match target_env.as_str() {
        "msvc" => compile_msvc(&rc_file, &manifest_dir, &out_dir)?,
        // Both windows-gnu and windows-gnullvm use COFF resource objects.
        _ => compile_gnu(&rc_file, &manifest_dir, &out_dir)?,
    }
    Ok(())
}

fn compile_msvc(rc_file: &Path, working_dir: &Path, out_dir: &Path) -> Result<(), Box<dyn Error>> {
    let output = out_dir.join("keysynth-resources.res");
    let compiler = env::var_os("RC").unwrap_or_else(|| OsStr::new("rc.exe").to_owned());
    run(
        Command::new(compiler)
            .arg("/nologo")
            .arg(format!("/fo{}", output.display()))
            .arg(rc_file)
            .current_dir(working_dir),
        "rc.exe",
    )?;
    println!("cargo:rustc-link-arg={}", output.display());
    Ok(())
}

fn compile_gnu(rc_file: &Path, working_dir: &Path, out_dir: &Path) -> Result<(), Box<dyn Error>> {
    let output = out_dir.join("keysynth-resources.o");
    let compiler = find_tool(&["LLVM_WINDRES", "WINDRES"], &["llvm-windres", "windres"])?;
    let mut command = Command::new(compiler);
    command
        .arg("--input-format=rc")
        .arg("--output-format=coff")
        .arg("--target=pe-x86-64")
        .arg("--input")
        .arg(rc_file)
        .arg("--output")
        .arg(&output)
        .current_dir(working_dir);
    run(&mut command, "llvm-windres/windres")?;
    println!("cargo:rustc-link-arg={}", output.display());
    Ok(())
}

fn find_tool(env_names: &[&str], candidates: &[&str]) -> Result<PathBuf, Box<dyn Error>> {
    for name in env_names {
        if let Some(path) = env::var_os(name) {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Ok(path);
            }
            return Err(format!(
                "{} points to a missing resource compiler: {}",
                name,
                path.display()
            )
            .into());
        }
    }
    for candidate in candidates {
        if let Some(path) = find_on_path(candidate) {
            return Ok(path);
        }
    }
    Err(
        "Windows resources require llvm-windres or windres on PATH (or LLVM_WINDRES/WINDRES)"
            .into(),
    )
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let extensions =
        env::var_os("PATHEXT").unwrap_or_else(|| OsStr::new(".EXE;.BAT;.CMD").to_owned());
    for directory in env::split_paths(&env::var_os("PATH")?) {
        let direct = directory.join(name);
        if direct.is_file() {
            return Some(direct);
        }
        for extension in extensions.to_string_lossy().split(';') {
            let path = directory.join(format!("{name}{extension}"));
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

fn run(command: &mut Command, display_name: &str) -> Result<(), Box<dyn Error>> {
    let status = command.stdin(Stdio::null()).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{display_name} failed while compiling Windows resources ({status})").into())
    }
}
