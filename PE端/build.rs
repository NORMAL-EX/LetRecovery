fn main() {
    // 把 vendor/libwim-15.dll 复制到最终可执行文件目录（target/<profile>/），
    // 使 wimlib 在运行时能在 exe 同目录找到它。
    copy_wimlib_dll();

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
        res.set("ProductVersion", "2026.2.6");
        res.set("FileVersion", "2026.2.6");

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
}

/// 将 vendor/libwim-15.dll 复制到 target/<profile>/ 及 deps/，供运行时加载。
fn copy_wimlib_dll() {
    use std::path::Path;

    let dll_name = "libwim-15.dll";
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let src = Path::new(&manifest_dir).join("vendor").join(dll_name);

    println!("cargo:rerun-if-changed=vendor/{}", dll_name);

    if !src.exists() {
        println!("cargo:warning=未找到 {}，跳过 DLL 复制", src.display());
        return;
    }

    // OUT_DIR 形如 target/<profile>/build/<pkg-hash>/out，向上 3 级即 target/<profile>
    let out_dir = match std::env::var("OUT_DIR") {
        Ok(d) => d,
        Err(_) => return,
    };
    let target_dir = match Path::new(&out_dir).ancestors().nth(3) {
        Some(d) => d.to_path_buf(),
        None => return,
    };

    let dst = target_dir.join(dll_name);
    if let Err(e) = std::fs::copy(&src, &dst) {
        println!("cargo:warning=复制 DLL 失败 {} -> {}: {}", src.display(), dst.display(), e);
    }

    let deps_dir = target_dir.join("deps");
    if deps_dir.exists() {
        let _ = std::fs::copy(&src, deps_dir.join(dll_name));
    }
}
