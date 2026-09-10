use std::fs;
use std::path::{Path, PathBuf};

fn crate_source(path: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(root.join(path)).unwrap_or_else(|error| panic!("read {path}: {error}"))
}

#[test]
fn frontend_federation_discovery_contract_matches_v2_only_backend() {
    let types =
        fs::read_to_string(repository_root().join("src/lib/types.ts")).expect("frontend types");
    let start = types
        .find("export interface FederationDiscoveryDocument {")
        .expect("FederationDiscoveryDocument");
    let end = types[start..]
        .find("export type FederationDiscoveryState")
        .map(|offset| start + offset)
        .expect("FederationDiscoveryState");
    let contract = &types[start..end];

    assert!(contract.contains("schemaVersion: 2;"));
    assert!(contract.contains("contract: \"anchor-federation-discovery-v2\";"));
    assert!(contract.contains("rotationChain?: FederationSigningRotationNotice[];"));
    assert!(!contract.contains("anchor-federation-discovery-v1"));
    assert!(!contract.contains("rotation?: FederationSigningRotationNotice;"));
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

#[test]
fn federation_discovery_has_one_v2_wire_contract() {
    let discovery = crate_source("src/federation/discovery.rs");
    let registry = crate_source("src/federation/registry.rs");
    let gateway = crate_source("src/mcp/gateway.rs");

    assert!(discovery.contains("const DISCOVERY_SCHEMA_VERSION: u16 = 2;"));
    assert!(discovery.contains("anchor-federation-discovery-v2"));
    assert!(!discovery.contains("LEGACY_DISCOVERY_SCHEMA_VERSION"));
    assert!(!discovery.contains("LEGACY_DISCOVERY_CONTRACT"));
    assert!(!discovery.contains("local_legacy_discovery_document"));
    assert!(!discovery.contains("pub rotation: Option<FederationSigningRotationNotice>"));
    assert!(!registry.contains("discovery.rotation.as_ref()"));
    assert!(gateway.contains("\"/federation/v2/discovery\""));
    assert!(!gateway.contains(".route(\"/federation/v2/bootstrap\""));
    assert!(!gateway.contains("federation_bootstrap_discovery"));
}

#[test]
fn federation_signing_has_no_v1_store_or_single_notice_bridge() {
    let signing = crate_source("src/federation/signing.rs");

    assert!(signing.contains("const SIGNING_IDENTITY_SCHEMA_VERSION: u16 = 2;"));
    assert!(signing.contains("federation-rotation-history.json"));
    assert!(!signing.contains("LEGACY_SIGNING_IDENTITY_SCHEMA_VERSION"));
    assert!(!signing.contains("struct LegacyStoredSigningIdentity"));
    assert!(!signing.contains("migrate_legacy_signing_identity"));
    assert!(!signing.contains("fn rotation_notice_path("));
    assert!(!signing.contains("atomic_write(&rotation_notice_path"));
}

#[test]
fn repository_keeps_generic_agent_skills_but_no_host_specific_skill_lanes() {
    let root = repository_root();
    let agents = fs::read_to_string(root.join("AGENTS.md")).expect("AGENTS.md");
    let claude = fs::read_to_string(root.join("CLAUDE.md")).expect("CLAUDE.md");
    let harness = crate_source("src/harness/tools.rs");

    assert!(root.join(".agents/skills/mcp-probe-kit/SKILL.md").is_file());
    assert!(!root.join(".claude").exists());
    assert!(!root.join(".cursor").exists());
    assert!(!agents.contains("GitNexus"));
    assert!(!agents.contains("gitnexus:"));
    assert!(!claude.contains("GitNexus"));
    assert!(!harness.contains("\".gitnexus\""));
    assert!(!harness.contains("path.starts_with(\".gitnexus/\")"));
}

#[test]
fn historical_specs_and_verification_have_explicit_archive_boundaries() {
    let root = repository_root();
    let docs_index = fs::read_to_string(root.join("docs/README.md")).expect("docs index");
    let specs = fs::read_to_string(root.join("docs/specs/README.md")).expect("specs archive");
    let verification =
        fs::read_to_string(root.join("docs/verification/README.md")).expect("verification archive");
    let federation = fs::read_to_string(root.join("docs/federation.md")).expect("federation docs");

    assert!(docs_index.contains("specs/README.md"));
    assert!(docs_index.contains("verification/README.md"));
    assert!(specs.contains("不是当前产品契约"));
    assert!(verification.contains("不是当前运行时契约"));
    assert!(federation.contains("不会在 404 或协议失败后回退到旧 `/federation/v2/bootstrap`"));
}
