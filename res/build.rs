fn main() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    println!("cargo:rerun-if-changed=res/app.manifest");
    println!("cargo:rerun-if-changed=res/app.rc");
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", dir.join("res").join("app.manifest").display());
    // version info: Task Manager lists the process as "Rust UI"
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("app.res");
    let kits = std::path::Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let rc = std::fs::read_dir(kits)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().join(r"x64\rc.exe"))
        .filter(|p| p.exists())
        .max();
    if let Some(rc) = rc {
        let inc = rc.parent().unwrap().parent().unwrap().parent().unwrap().parent().unwrap().join("Include");
        let ver = rc.parent().unwrap().parent().unwrap().file_name().unwrap().to_owned();
        let um = inc.join(&ver).join("um");
        let shared = inc.join(&ver).join("shared");
        let ok = std::process::Command::new(&rc)
            .args(["/nologo", "/i"]).arg(&um).arg("/i").arg(&shared).arg("/fo").arg(&out).arg(dir.join("res").join("app.rc"))
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            println!("cargo:rustc-link-arg-bins={}", out.display());
        }
    }
}
