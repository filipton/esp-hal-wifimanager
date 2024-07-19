fn main() {
    let path_env = std::env::var("PATH").expect("NO PATH ENV FOUND!");
    let rustup_home_env = std::env::var("RUSTUP_HOME").expect("NO RUSTUP_HOME ENV FOUND!");
    let esp_toolchains_path = std::path::PathBuf::from(rustup_home_env)
        .join("toolchains")
        .join("esp");

    if !esp_toolchains_path.exists() {
        panic!(
            ".rustup/toolchains/esp path not found! Are you sure you've installed esp toolchains?"
        );
    }

    let clang_path = get_newest(&esp_toolchains_path.join("xtensa-esp32-elf-clang")).expect("xtensa-esp32-elf-clang not found in esp toolchains! Are you sure you've installed esp toolchains?");
    let elf_path = get_newest(&esp_toolchains_path.join("xtensa-esp-elf")).expect(
        "xtensa-esp-elf not found in esp toolchains! Are you sure you've installed esp toolchains?",
    );

    println!(
        "cargo:rustc-env=LIBCLANG_PATH={}/esp-clang/lib",
        clang_path.display()
    );
    println!(
        "cargo:rustc-env=PATH={}/xtensa-esp-elf/bin:{path_env}",
        elf_path.display()
    );

    println!("cargo:rerun-if-changed=*.env*");
    if let Ok(mut iter) = dotenvy::dotenv_iter() {
        while let Some(Ok((key, value))) = iter.next() {
            println!("cargo:rustc-env={key}={value}");
        }
    }

    println!("cargo:rustc-link-arg-bins=-Tlinkall.x");
    println!("cargo:rustc-link-arg-bins=-Trom_functions.x");
}

fn get_newest(path: &std::path::PathBuf) -> Option<std::path::PathBuf> {
    if !path.exists() {
        return None;
    }

    let mut tmp_path = (None, None);
    for entry in path.read_dir().unwrap() {
        if let Ok(entry) = entry {
            let metadata = entry.metadata().expect("Cant get dir metadata!");
            let modified = metadata.modified().expect("Cant get dir modified time!");

            match tmp_path.0 {
                Some(old) => {
                    if modified > old {
                        tmp_path = (Some(modified), Some(entry.path()));
                    }
                }
                None => {
                    tmp_path = (Some(modified), Some(entry.path()));
                }
            }
        }
    }

    tmp_path.1
}
