fn main() {
    println!("cargo:rerun-if-changed=src/process_zombie.c");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("src/process_zombie.c")
            .warnings_into_errors(true)
            .compile("hive_process_zombie");
    }
}
