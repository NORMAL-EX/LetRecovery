fn main() {
    // 注：libwim-15.dll 已内置于共享库 lr-core，运行时自动释放到 exe 目录，
    // 这里不再需要从 vendor 复制。

    // 按编译日期自动生成版本号（无需每次手动改版本）
    let (y, m, d) = build_date();
    let display_version = format!("v{}.{:02}.{:02}", y, m, d);
    let numeric_version = format!("{}.{}.{}.0", y, m, d);
    println!("cargo:rustc-env=BUILD_VERSION={}", display_version);

    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();

        // 设置程序图标
        if std::path::Path::new("assets/icon.ico").exists() {
            res.set_icon("assets/icon.ico");
        }

        // 设置程序信息
        res.set("ProductName", "LetRecovery PE");
        res.set("FileDescription", "LetRecovery PE安装助手");
        res.set("LegalCopyright", "Copyright © 2026 NORMAL-EX");
        res.set("ProductVersion", &numeric_version);
        res.set("FileVersion", &numeric_version);

        // 关键：同时写入二进制 FIXEDFILEINFO 版本号。
        // 资源管理器“文件版本”读取的是 FIXEDFILEINFO，而 winres 默认用
        // CARGO_PKG_VERSION（Cargo.toml 的包版本）填充，导致文件版本一直停在旧日期。
        // 这里按编译日期覆盖，确保“文件版本/产品版本”都跟随编译日期。
        let ver_u64: u64 =
            ((y as u64 & 0xffff) << 48) | ((m as u64) << 32) | ((d as u64) << 16);
        res.set_version_info(winres::VersionInfo::FILEVERSION, ver_u64);
        res.set_version_info(winres::VersionInfo::PRODUCTVERSION, ver_u64);

        // 包含 Common Controls 6.0 和管理员权限
        res.set_manifest(r#"
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
"#);

        if let Err(e) = res.compile() {
            eprintln!("Warning: Failed to compile Windows resources: {}", e);
        }
    }

    #[cfg(not(windows))]
    let _ = numeric_version;
}

/// 取当前 UTC 日期 (年, 月, 日)，无第三方依赖。
fn build_date() -> (i64, u32, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86400);
    civil_from_days(days)
}

/// 天数(自 1970-01-01) -> (年, 月, 日)，Howard Hinnant 算法。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as i64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
