//! Local runtime state: data-root paths, host identity, config, session
//! registrations, operation journals, invalidation watching and the steward.

pub mod checks;
pub mod identity;
pub mod install;
pub mod local_state;
pub mod paths;
pub mod presence;
pub mod sessions;
pub mod shown;
pub mod steward;
pub mod usage;
pub mod watch;

pub use checks::{
    CheckApproval, CheckProfile, CheckRun, approve as approve_check, load as load_check_approval,
    profile_digest, run as run_check, validate_cwd,
};
pub use identity::HostIdentity;
pub use install::{
    HookChange, InstallPlan, claude_settings_path, home_dir, install_claude_hooks,
    install_path_link, plan_claude_hooks, plan_path_link,
};
pub use local_state::{
    Config, LOCK_WAIT, MAX_LOCAL_JSON_BYTES, acquire_lock, acquire_lock_until, ensure_dir,
    lock_wait, read_json_file, read_json_value, set_lock_wait, sync_dir, write_bytes_atomic,
    write_json_atomic,
};
pub use paths::{LocalLayout, LocalPaths};
pub use sessions::{
    LocalSession, OperationEntry, OperationRecord, OperationStatus, RuntimeState, WaitLease,
    new_event_id, new_session_id, process_identity, update_runtime, update_runtime_until,
};
pub use steward::{
    CLAUDE_RESUME_GRACE_S, SessionRegistration, StewardReport, register_session, steward_tick,
    steward_tick_at,
};
pub use watch::{Changes, RECONCILE_INTERVAL, VaultWatch, WaitOutcome};
