use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

// Write better one in future, good enough for now
fn main() {
    let profile = env::var("PROFILE").unwrap_or_default();
    if profile != "release" {
        return;
    }

    let target_cpu = if env::var("RSMALLOC_NATIVE").is_ok() {
        "native".to_string()
    } else {
        detect_x86_64_tier()
    };

    println!(
        "cargo:warning=🚀 rsmalloc configuring .cargo/config.toml with target-cpu={}",
        target_cpu
    );
    println!("cargo:rerun-if-env-changed=RSMALLOC_NATIVE");

    let config_content = format!(
        r#"[build]
rustflags = [
    "-C", "target-cpu={}",
    "-C", "link-arg=-Wl,-z,now",
    "-Z", "tls-model=initial-exec",
    "-C", "force-unwind-tables=no",
    "-C", "llvm-args=-align-all-functions=5",
    "-C", "llvm-args=-x86-pad-for-align=false",
    "-C", "llvm-args=--inline-threshold=275",
    "-C", "code-model=small"
]
"#,
        target_cpu
    );

    let dot_cargo_dir = Path::new(".cargo");
    if !dot_cargo_dir.exists() {
        fs::create_dir(dot_cargo_dir).expect("Failed to create .cargo directory");
    }

    fs::write(dot_cargo_dir.join("config.toml"), config_content)
        .expect("Failed to write .cargo/config.toml");
}

fn detect_x86_64_tier() -> String {
    let output = Command::new("sh")
        .arg("-c")
        .arg("grep -m 1 flags /proc/cpuinfo")
        .output();

    if let Ok(out) = output {
        let flags = String::from_utf8_lossy(&out.stdout);

        if flags.contains("avx512f") && flags.contains("avx512vl") && flags.contains("avx512bw") {
            return "x86-64-v4".to_string();
        } else if flags.contains("avx2") && flags.contains("bmi2") && flags.contains("fma") {
            return "x86-64-v3".to_string();
        } else if flags.contains("popcnt") && flags.contains("sse4_2") {
            return "x86-64-v2".to_string();
        }
    }

    "x86-64".to_string()
}
