fn main() {
    let commit = git_commit();
    println!("cargo:rustc-env=PDF_MEASURE_COMMIT={commit}");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}

fn git_commit() -> String {
    let Ok(out) = std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
    else {
        return "unknown".to_string();
    };
    if !out.status.success() {
        return "unknown".to_string();
    }
    let Ok(text) = String::from_utf8(out.stdout) else {
        return "unknown".to_string();
    };
    let text = text.trim();
    if text.is_empty() {
        return "unknown".to_string();
    }
    let dirty = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .is_some_and(|text| !text.trim().is_empty());
    if dirty {
        format!("{text}-dirty")
    } else {
        text.to_string()
    }
}
