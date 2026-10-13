fn main() {
    // Build date (UTC) for Settings › About.
    let days = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 / 86_400;
    let (y, m, d) = civil_from_days(days);
    println!("cargo:rustc-env=KILATX_BUILD_DATE={y:04}-{m:02}-{d:02}");

    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles);
    slint_build::compile_with_config("ui/app.slint", config).unwrap();

    // DLLs that KilatX.exe imports load only from System32, never from its own folder
    // (where another program could plant a fake one). See also setup_hooks::harden_dll_search.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bin=KilatX=/DEPENDENTLOADFLAG:0x800");
    }

    // Icon and file details shown by Explorer for KilatX.exe.
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("ui/icons/app.ico")
            .set("ProductName", "KilatX")
            .set("FileDescription", "KilatX download manager");
        res.compile().unwrap();
    }
}

/// Days since 1970-01-01 to (year, month, day); Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}
