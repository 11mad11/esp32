use advmac::MacAddr6;
use dotenv::dotenv;
use std::{fs, io::Write, process::Command};

fn main() {
    linker_be_nice();
    println!("cargo:rustc-link-arg=-Tdefmt.x");
    // make sure linkall.x is the last linker script (otherwise might cause problems with flip-link)
    println!("cargo:rustc-link-arg=-Tlinkall.x");

    //let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    //println!("cargo:rustc-link-search={}", out.display());
    //fs::copy("memory.x", out.join("memory.x")).unwrap();
    //fs::write("test.txt", out.display().to_string()).unwrap();
    //println!("cargo:rustc-link-arg=-Tmemory.x");
    println!("cargo:rustc-link-arg=-Wl,-Map=output.map");

    {
        let output = Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output();

        let hash = match output {
            Ok(output) => String::from_utf8(output.stdout)
                .ok()
                .and_then(|s| s.trim().split('\n').next().map(|s| s.to_string())),
            Err(_) => None,
        };

        let hash = hash.as_deref().unwrap_or("0000000");

        println!("cargo:rustc-env=GIT_HASH={}", hash);
    }

    let mut env_file = {
        let env_path = std::env::current_dir()
            .expect("Failed to get current directory")
            .join(".env");
        if !env_path.exists() {
            fs::File::create(env_path.clone()).expect("Failed to create .env file");
        }
        println!("cargo:rerun-if-changed={}",env_path.as_path().display());
        dotenv().unwrap();
        fs::OpenOptions::new()
            .write(true)
            .append(true)
            .open(env_path)
            .expect("Failed to open .env file")
    };

    if let Ok(value) = std::env::var("TOKEN") {
        println!("cargo:rustc-env=TOKEN={}", value);
    }
    if let Ok(value) = std::env::var("ID") {
        println!("cargo:rustc-env=ID={}", value);
    }
    {
        let mac = std::env::var("MAC").ok().unwrap_or_else(|| {
            let mac = generate_random_mac();
            writeln!(env_file, "\nMAC={}", mac).expect("Failed to write MAC to .env file");
            mac
        });
        println!("cargo:rustc-env=MAC={}", mac);
    }
}

fn generate_random_mac() -> String {
    let mut mac = MacAddr6::random();
    mac.set_local(true);
    mac.format_string(advmac::MacAddrFormat::ColonNotation)
}

fn linker_be_nice() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let kind = &args[1];
        let what = &args[2];

        match kind.as_str() {
            "undefined-symbol" => match what.as_str() {
                "_defmt_timestamp" => {
                    eprintln!();
                    eprintln!("💡 `defmt` not found - make sure `defmt.x` is added as a linker script and you have included `use defmt_rtt as _;`");
                    eprintln!();
                }
                "_stack_start" => {
                    eprintln!();
                    eprintln!("💡 Is the linker script `linkall.x` missing?");
                    eprintln!();
                }
                _ => (),
            },
            // we don't have anything helpful for "missing-lib" yet
            _ => {
                std::process::exit(1);
            }
        }

        std::process::exit(0);
    }

    println!(
        "cargo:rustc-link-arg=-Wl,--error-handling-script={}",
        std::env::current_exe().unwrap().display()
    );
}
