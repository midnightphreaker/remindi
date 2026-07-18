use std::{path::Path, process::Command};

#[test]
fn supported_compose_boundary_is_valid_and_hardened() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dockerfile = std::fs::read_to_string(root.join("Dockerfile")).expect("Dockerfile");
    let compose = std::fs::read_to_string(root.join("compose.yaml")).expect("compose.yaml");
    let readme = std::fs::read_to_string(root.join("README.md")).expect("README.md");

    assert!(dockerfile.contains("FROM rust:1.97.1-bookworm AS builder"));
    assert!(dockerfile.contains("COPY migrations ./migrations"));
    assert!(dockerfile.contains("cargo build --locked --release --bin remindi"));
    assert!(dockerfile.contains("USER 10001:10001"));
    assert!(dockerfile.contains("VOLUME [\"/data\"]"));
    assert!(dockerfile.contains("EXPOSE 8000"));
    assert!(dockerfile.contains("/health/live"));

    assert!(compose.contains("${REMINDI_WEBUI_HOST:-127.0.0.1}"));
    assert!(compose.contains("${REMINDI_WEBUI_PORT:-8000}:8000"));
    assert!(compose.contains("read_only: true"));
    assert!(compose.contains("no-new-privileges:true"));
    assert!(compose.contains("cap_drop:"));
    assert!(compose.contains("- remindi-data:/data"));
    assert!(!compose.contains("docker.sock"));
    assert!(!compose.contains("privileged:"));

    assert!(readme.contains("UID/GID `10001:10001`"));
    assert!(readme.contains("Do not"));
    assert!(readme.contains("copy a live SQLite database file."));
    assert!(readme.contains("## Agent pull workflow"));
    assert!(readme.contains("one write-capable"));
    assert!(readme.contains("per database."));

    let output = Command::new("docker")
        .args(["compose", "-f", "compose.yaml", "config", "--quiet"])
        .current_dir(root)
        .env("REMINDI_OWNER_ID", "docker-contract-owner")
        .env("REMINDI_MCP_TOKEN", "docker-contract-token")
        .env("REMINDI_WEBUI_AUTH", "false")
        .output()
        .expect("Docker Compose must be installed for the Docker acceptance suite");
    assert!(
        output.status.success(),
        "docker compose config failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
