fn main() {
    println!("cargo:rerun-if-changed=.env");
    if let Ok(iter) = dotenvy::dotenv_iter() {
        for (key, value) in iter.flatten() {
            println!("cargo:rustc-env={key}={value}");
        }
    }

    println!("cargo:rustc-link-arg-bins=-Tlinkall.x");
}
