use super::*;
use autoreport_protocol::protocol_types::SandboxPolicy;

#[test]
fn base_policy_allows_node_cpu_sysctls() {
    assert!(
        MACOS_SEATBELT_BASE_POLICY.contains("(sysctl-name \"machdep.cpu.brand_string\")"),
        "base policy must allow CPU brand lookup for os.cpus()"
    );
    assert!(
        MACOS_SEATBELT_BASE_POLICY.contains("(sysctl-name \"hw.model\")"),
        "base policy must allow hardware model lookup for os.cpus()"
    );
}

#[test]
fn base_policy_allows_kmp_registration_shm_read_create_and_unlink() {
    let expected = r##"(allow ipc-posix-shm-read-data
  ipc-posix-shm-write-create
  ipc-posix-shm-write-unlink
  (ipc-posix-name-regex #"^/__KMP_REGISTERED_LIB_[0-9]+$"))"##;

    assert!(
        MACOS_SEATBELT_BASE_POLICY.contains(expected),
        "base policy must allow only KMP registration shared memory operations:\n{MACOS_SEATBELT_BASE_POLICY}"
    );
}

#[test]
fn dynamic_network_policy_routes_through_proxy_ports() {
    let policy = dynamic_network_policy(
        &SandboxPolicy::new_read_only_policy(),
        /*enforce_managed_network*/ false,
        &ProxyPolicyInputs {
            ports: vec![43128, 48081],
            has_proxy_config: true,
            allow_local_binding: false,
            ..ProxyPolicyInputs::default()
        },
    );

    assert!(
        policy.contains("(allow network-outbound (remote ip \"localhost:43128\"))"),
        "expected HTTP proxy port allow rule in policy:\n{policy}"
    );
    assert!(
        policy.contains("(allow network-outbound (remote ip \"localhost:48081\"))"),
        "expected SOCKS proxy port allow rule in policy:\n{policy}"
    );
    assert!(
        !policy.contains("\n(allow network-outbound)\n"),
        "policy should not include blanket outbound allowance when proxy ports are present:\n{policy}"
    );
    assert!(
        !policy.contains("(allow network-bind (local ip \"*:*\"))"),
        "policy should not allow local binding unless explicitly enabled:\n{policy}"
    );
}

#[test]
fn dynamic_network_policy_keeps_tls_support_without_user_cache_write() {
    let policy = dynamic_network_policy(
        &SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            network_access: true,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        },
        /*enforce_managed_network*/ false,
        &ProxyPolicyInputs::default(),
    );

    assert!(
        policy.contains("(global-name \"com.apple.trustd.agent\")"),
        "policy should keep trustd agent access for TLS certificate verification:\n{policy}"
    );
    assert!(
        !policy.contains("DARWIN_USER_CACHE_DIR"),
        "network policy should not grant broad user cache writes:\n{policy}"
    );
}
