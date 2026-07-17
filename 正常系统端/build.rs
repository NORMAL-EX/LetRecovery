fn main() {
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    println!("cargo:rerun-if-changed=assets/icon.png");
    println!("cargo:rerun-if-changed=assets/win11_button_theme");

    // 注：libwim-15.dll 已内置于共享库 lr-core，运行时自动释放到 exe 目录，
    // 这里不再需要从 vendor 复制。

    // 按编译日期自动生成版本号（无需每次手动改版本）
    let (y, m, d) = build_date();
    let display_version = format!("v{}.{:02}.{:02}", y, m, d); // 如 v2026.06.07
    let numeric_version = format!("{}.{}.{}.0", y, m, d); // winres 需要 n.n.n.n
                                                          // 注入到编译环境，供代码用 env!("BUILD_VERSION") 读取
    println!("cargo:rustc-env=BUILD_VERSION={}", display_version);

    // 仅在 Windows 上设置资源
    #[cfg(windows)]
    {
        generate_win11_button_theme();
        let non_elevated_tests = std::env::var_os("CARGO_FEATURE_NON_ELEVATED_TESTS").is_some();
        if non_elevated_tests && std::env::var("PROFILE").as_deref() == Ok("release") {
            panic!("non-elevated-tests must never be enabled for release builds");
        }

        let mut res = winres::WindowsResource::new();

        // 仓库历史 icon.ico 无法被 Windows 稳定解析。始终从 PNG 生成合法 ICO，
        // 避免窗口类和 EXE 文件属性退回系统默认应用图标。
        let generated_icon = generate_icon_from_png();
        res.set_icon(
            generated_icon
                .to_str()
                .expect("generated icon path is UTF-8"),
        );

        // 设置程序信息
        res.set("ProductName", "LetRecovery");
        res.set("FileDescription", "Windows系统一键重装工具");
        res.set("LegalCopyright", "Copyright (C) 2026 NORMAL-EX");
        res.set("ProductVersion", &numeric_version);
        res.set("FileVersion", &numeric_version);

        // 关键：同时写入二进制 FIXEDFILEINFO 版本号。
        // 资源管理器“文件版本”读取的是 FIXEDFILEINFO，而 winres 默认用
        // CARGO_PKG_VERSION（Cargo.toml 的包版本）填充，导致文件版本一直停在旧日期。
        // 这里按编译日期覆盖，确保“文件版本/产品版本”都跟随编译日期。
        let ver_u64: u64 = ((y as u64 & 0xffff) << 48) | ((m as u64) << 32) | ((d as u64) << 16);
        res.set_version_info(winres::VersionInfo::FILEVERSION, ver_u64);
        res.set_version_info(winres::VersionInfo::PRODUCTVERSION, ver_u64);

        // 正式程序请求管理员权限；纯测试特性使用 asInvoker，避免测试 EXE
        // 在无交互 CI/本地终端中因 UAC 清单而无法启动。
        let manifest = r#"
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
    <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
        <security>
            <requestedPrivileges>
                <requestedExecutionLevel level="requireAdministrator" uiAccess="false"/>
            </requestedPrivileges>
        </security>
    </trustInfo>
    <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
        <application>
            <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
            <supportedOS Id="{1f676c76-80e1-4239-95bb-83d0f6d0da78}"/>
            <supportedOS Id="{4a2f28e3-53b9-4441-ba9c-d69d4a4a6e38}"/>
            <supportedOS Id="{35138b9a-5d96-4fbd-8e2d-a2440225f93a}"/>
            <supportedOS Id="{e2011457-1546-43c5-a5fe-008deee3d3f0}"/>
        </application>
    </compatibility>
    <dependency>
        <dependentAssembly>
            <assemblyIdentity
                type="win32"
                name="Microsoft.Windows.Common-Controls"
                version="6.0.0.0"
                processorArchitecture="*"
                publicKeyToken="6595b64144ccf1df"
                language="*"
            />
        </dependentAssembly>
    </dependency>
</assembly>
"#;
        let manifest = if non_elevated_tests {
            manifest.replace("requireAdministrator", "asInvoker")
        } else {
            manifest.to_string()
        };
        res.set_manifest(&manifest);

        res.compile()
            .expect("failed to compile required Windows resources and elevation manifest");
    }

    // 非 Windows 平台也要消除未使用变量告警
    #[cfg(not(windows))]
    let _ = numeric_version;
}

#[cfg(windows)]
fn generate_win11_button_theme() {
    use std::fmt::Write as _;

    const DPIS: [u32; 4] = [96, 120, 144, 192];
    const MODES: [(&str, bool); 2] = [("light", false), ("dark", true)];
    let mut source = String::from(
        "// Generated from assets/win11_button_theme by build.rs; do not edit by hand.\n\
         static WIN11_CHECKBOX_THEME_GLYPHS: [EmbeddedButtonGlyph; 64] = [\n",
    );
    for (mode, _dark) in MODES {
        for dpi in DPIS {
            for state in 1..=8 {
                let path = format!("assets/win11_button_theme/{mode}-{dpi}-checkbox-{state}.png");
                let rgba = image::open(&path)
                    .unwrap_or_else(|error| panic!("failed to open {path}: {error}"))
                    .into_rgba8();
                let (width, height) = rgba.dimensions();
                let mut bgra = Vec::with_capacity(rgba.as_raw().len());
                for pixel in rgba.as_raw().chunks_exact(4) {
                    bgra.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
                }
                write!(
                    source,
                    "    EmbeddedButtonGlyph {{ width: {width}, height: {height}, bgra: &["
                )
                .expect("write generated button theme header");
                for byte in bgra {
                    write!(source, "{byte},").expect("write generated button theme pixel");
                }
                source.push_str("] },\n");
            }
        }
    }
    source.push_str("];\n");

    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"))
        .join("win11_button_theme.rs");
    std::fs::write(output, source).expect("write generated Win11 button theme table");
}

#[cfg(windows)]
fn generate_icon_from_png() -> std::path::PathBuf {
    use image::imageops::FilterType;
    use image::{ImageEncoder, RgbaImage};
    use std::io::Write;

    let source = image::open("assets/icon.png")
        .expect("failed to open assets/icon.png")
        .into_rgba8();
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"))
        .join("LetRecovery.generated.ico");
    const SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];
    let mut images = Vec::with_capacity(SIZES.len());
    for size in SIZES {
        let resized: RgbaImage = image::imageops::resize(&source, size, size, FilterType::Lanczos3);
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(
                resized.as_raw(),
                size,
                size,
                image::ExtendedColorType::Rgba8,
            )
            .expect("failed to encode generated icon frame");
        images.push((size, png));
    }

    let directory_bytes = 6 + 16 * images.len();
    let total_image_bytes: usize = images.iter().map(|(_, png)| png.len()).sum();
    let mut bytes = Vec::with_capacity(directory_bytes + total_image_bytes);
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = directory_bytes as u32;
    for (size, png) in &images {
        let dimension = if *size == 256 { 0 } else { *size as u8 };
        bytes.extend_from_slice(&[dimension, dimension, 0, 0]);
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&32u16.to_le_bytes());
        bytes.extend_from_slice(&(png.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in images {
        bytes.extend_from_slice(&png);
    }
    let mut file = std::fs::File::create(&output).expect("failed to create generated icon");
    file.write_all(&bytes)
        .expect("failed to encode generated icon");
    output
}

/// 取当前 UTC 日期 (年, 月, 日)，无第三方依赖。
fn build_date() -> (i64, u32, u32) {
    let secs = build_timestamp();
    let days = secs.div_euclid(86400);
    civil_from_days(days)
}

fn build_timestamp() -> i64 {
    if let Some(value) = std::env::var_os("SOURCE_DATE_EPOCH") {
        let value = value
            .to_str()
            .expect("SOURCE_DATE_EPOCH must contain ASCII decimal digits");
        let timestamp = value
            .parse::<i64>()
            .expect("SOURCE_DATE_EPOCH must be a valid Unix timestamp");
        assert!(timestamp >= 0, "SOURCE_DATE_EPOCH must not be negative");
        return timestamp;
    }

    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// 天数(自 1970-01-01) -> (年, 月, 日)，Howard Hinnant 算法。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}
