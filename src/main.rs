use cargo_metadata::{camino::Utf8PathBuf, Message};
use clap::{Args, Parser, Subcommand};
use fs_err as fs;
use std::{
    io::{self, ErrorKind},
    path::PathBuf,
    process::{exit, Child, Command, Stdio},
};

cargo_subcommand_metadata::description!("Manage pros-rs projects");

#[derive(Parser, Debug)]
#[clap(bin_name = "cargo")]
enum Cli {
    /// Manage pros-rs projects
    #[clap(version)]
    Pros(Opt),
}

#[derive(Args, Debug)]
struct Opt {
    #[command(subcommand)]
    command: Commands,

    #[arg(long, default_value = ".")]
    path: PathBuf,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Build {
        #[clap(long, short)]
        simulator: bool,
        #[clap(last = true)]
        args: Vec<String>,
    },
    Simulate {
        #[clap(last = true)]
        args: Vec<String>,
    },
}

fn cargo_bin() -> std::ffi::OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| "cargo".to_owned().into())
}

trait CommandExt {
    fn spawn_handling_not_found(&mut self) -> io::Result<Child>;
}

impl CommandExt for Command {
    fn spawn_handling_not_found(&mut self) -> io::Result<Child> {
        let command_name = self.get_program().to_string_lossy().to_string();
        self.spawn().map_err(|err| match err.kind() {
            ErrorKind::NotFound => {
                eprintln!("error: command `{}` not found", command_name);
                eprintln!(
                    "Please refer to the documentation for installing pros-rs on your platform."
                );
                eprintln!("> https://github.com/pros-rs/pros-rs#compiling");
                exit(1);
            }
            _ => err,
        })
    }
}

const TARGET_PATH: &str = "target/armv7a-vexos-eabi.json";

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Cli::Pros(args) = Cli::parse();
    let path = args.path;

    if !is_nightly_toolchain() {
        eprintln!("warn: pros-rs currently requires Nightly Rust features.");
        eprintln!("hint: this can be fixed by running `rustup override set nightly`");
        exit(1);
    }

    match args.command {
        Commands::Build { simulator, args } => {
            build(path, args, simulator, |path| {
                if !simulator {
                    strip_binary(path);
                }
            });
        }
        Commands::Simulate { args } => {
            let mut wasm_path = None;
            build(path, args, true, |path| wasm_path = Some(path));
            let wasm_path = wasm_path.expect("pros-simulator may not run libraries");

            let mut connection = jsonl::Connection::new_from_stdio();
            pros_simulator::simulate(wasm_path.as_std_path(), move |event| {
                connection.write(&event).unwrap();
            })
            .await
            .unwrap();
        }
    }

    Ok(())
}

fn build(
    path: PathBuf,
    args: Vec<String>,
    for_simulator: bool,
    mut handle_executable: impl FnMut(Utf8PathBuf),
) {
    let target_path = path.join(TARGET_PATH);
    let mut build_cmd = Command::new(cargo_bin());
    build_cmd
        .current_dir(&path)
        .arg("build")
        .arg("--message-format")
        .arg("json-render-diagnostics")
        .arg("--manifest-path")
        .arg(format!("{}/Cargo.toml", path.display()));

    if !is_nightly_toolchain() {
        eprintln!("warn: pros-rs currently requires Nightly Rust features.");
        eprintln!("hint: this can be fixed by running `rustup override set nightly`");
        exit(1);
    }

    if for_simulator {
        if !has_wasm_target() {
            eprintln!(
                "warn: simulation requires the wasm32-unknown-unknown target to be installed"
            );
            eprintln!(
                "hint: this can be fixed by running `rustup target add wasm32-unknown-unknown`"
            );
            exit(1);
        }

        build_cmd
            .arg("--target")
            .arg("wasm32-unknown-unknown")
            .arg("-Zbuild-std=std,panic_abort")
            .arg("--config=build.rustflags=['-Ctarget-feature=+atomics,+bulk-memory,+mutable-globals','-Clink-arg=--shared-memory','-Clink-arg=--export-table']")
            .stdout(Stdio::piped());
    } else {
        let target = include_str!("armv7a-vexos-eabi.json");
        fs::create_dir_all(target_path.parent().unwrap()).unwrap();
        fs::write(&target_path, target).unwrap();
        build_cmd.arg("--target");
        build_cmd.arg(&target_path);

        build_cmd
            .arg("-Zbuild-std=core,alloc,compiler_builtins")
            .stdout(Stdio::piped());
    }

    build_cmd.args(args);

    let mut out = build_cmd.spawn_handling_not_found().unwrap();
    let reader = std::io::BufReader::new(out.stdout.take().unwrap());
    for message in Message::parse_stream(reader) {
        if let Message::CompilerArtifact(artifact) = message.unwrap() {
            if let Some(binary_path) = artifact.executable {
                handle_executable(binary_path);
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn find_objcopy_path_windows() -> Option<String> {
    let arm_install_path =
        PathBuf::from("C:\\Program Files (x86)\\Arm GNU Toolchain arm-none-eabi");
    let mut versions = fs::read_dir(arm_install_path).ok()?;
    let install = versions.next()?.ok()?.path();
    let path = install.join("bin").join("arm-none-eabi-objcopy.exe");
    Some(path.to_string_lossy().to_string())
}

fn objcopy_path() -> String {
    #[cfg(target_os = "windows")]
    let objcopy_path = find_objcopy_path_windows();

    #[cfg(not(target_os = "windows"))]
    let objcopy_path = None;

    objcopy_path.unwrap_or_else(|| "arm-none-eabi-objcopy".to_owned())
}

fn strip_binary(bin: Utf8PathBuf) {
    println!("Stripping Binary: {}", bin.clone());
    let objcopy = objcopy_path();
    let strip = std::process::Command::new(&objcopy)
        .args([
            "--strip-symbol=install_hot_table",
            "--strip-symbol=__libc_init_array",
            "--strip-symbol=_PROS_COMPILE_DIRECTORY",
            "--strip-symbol=_PROS_COMPILE_TIMESTAMP",
            "--strip-symbol=_PROS_COMPILE_TIMESTAMP_INT",
            bin.as_str(),
            &format!("{}.stripped", bin),
        ])
        .spawn_handling_not_found()
        .unwrap();
    strip.wait_with_output().unwrap();
    let elf_to_bin = std::process::Command::new(&objcopy)
        .args([
            "-O",
            "binary",
            "-R",
            ".hot_init",
            &format!("{}.stripped", bin),
            &format!("{}.bin", bin),
        ])
        .spawn_handling_not_found()
        .unwrap();
    elf_to_bin.wait_with_output().unwrap();
}

fn is_nightly_toolchain() -> bool {
    let rustc = std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .unwrap();
    let rustc = String::from_utf8(rustc.stdout).unwrap();
    rustc.contains("nightly")
}

fn has_wasm_target() -> bool {
    let Ok(rustup) = std::process::Command::new("rustup")
        .arg("target")
        .arg("list")
        .arg("--installed")
        .output()
    else {
        return true;
    };
    let rustup = String::from_utf8(rustup.stdout).unwrap();
    rustup.contains("wasm32-unknown-unknown")
}
