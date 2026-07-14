//! Protocol types required by the vendored sandbox sources.
//!
//! Split filesystem policy model ([`permissions`]) plus the legacy
//! [`protocol_types`] (`SandboxPolicy` / `WritableRoot` / `NetworkAccess`),
//! the [`models`] permission-profile layer, [`config_types`], [`error`], and
//! [`exec_output`]. Verbatim from `codex-rs/protocol/src`.

pub mod config_types;
pub mod error;
pub mod exec_output;
pub mod models;
pub mod permissions;
pub mod protocol_types;

pub use config_types::WindowsSandboxLevel;
pub use error::CodexErr;
pub use exec_output::ExecToolCallOutput;
pub use exec_output::StreamOutput;
pub use models::AdditionalPermissionProfile;
pub use models::FileSystemPermissions;
pub use models::ManagedFileSystemPermissions;
pub use models::NetworkPermissions;
pub use models::PermissionProfile;
pub use models::SandboxEnforcement;
pub use models::SandboxPermissions;
pub use permissions::FileSystemAccessMode;
pub use permissions::FileSystemPath;
pub use permissions::FileSystemSandboxEntry;
pub use permissions::FileSystemSandboxKind;
pub use permissions::FileSystemSandboxPolicy;
pub use permissions::FileSystemSpecialPath;
pub use permissions::NetworkSandboxPolicy;
pub use permissions::PROTECTED_METADATA_PATH_NAMES;
pub use permissions::ReadDenyMatcher;
pub use protocol_types::NetworkAccess;
pub use protocol_types::SandboxPolicy;
pub use protocol_types::WritableRoot;
