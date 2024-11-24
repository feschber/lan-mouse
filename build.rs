use std::process::Command;

fn main() {
    // commit hash
    let git_describe = Command::new("git")
        .arg("describe")
        .arg("--always")
        .arg("--dirty")
        .arg("--tags")
        .output()
        .map(|output| String::from_utf8(output.stdout).ok())
        .ok()
        .flatten()
        .unwrap_or_else(|| {
            println!("cargo:warning=Failed to get git describe");
            String::from("unknown")
        });
    let git_describe = git_describe.trim().to_string();
    println!("cargo::rustc-env=GIT_DESCRIBE={git_describe}");
}
