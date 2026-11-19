//! Reading a Vault kept in iCloud Drive.
//!
//! macOS evicts iCloud files it has not used lately ("dataless" files, more
//! often when the disk is nearly full). Reading one downloads it first, unless
//! the process runs with the materialization policy off: then every read fails
//! at once with `EDEADLK` ("Resource deadlock avoided"). Processes inherit the
//! policy from whatever started them, so a terminal or agent harness that turns
//! it off for itself turns it off for every hook and CLI call it starts.
//!
//! [`allow_downloads`] turns the policy back on for this process (and the
//! processes it starts). Agendar's entry points call it first thing.
//!
//! The API is safe; the one `unsafe` block is the `setiopolicy_np` call
//! below (one of the allowed `unsafe` exceptions, see `AGENTS.md`).

#![deny(unsafe_op_in_unsafe_fn)]

/// Lets reads in this process download evicted iCloud files. Returns whether
/// the policy is now on; `false` on other platforms and when macOS refused,
/// which leaves reads of evicted files failing as before (the Vault error then
/// says so).
pub fn allow_downloads() -> bool {
    imp::allow_downloads()
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_int;

    // <sys/resource.h>
    const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: c_int = 3;
    const IOPOL_SCOPE_PROCESS: c_int = 0;
    const IOPOL_MATERIALIZE_DATALESS_FILES_ON: c_int = 2;

    unsafe extern "C" {
        fn setiopolicy_np(iotype: c_int, scope: c_int, policy: c_int) -> c_int;
    }

    pub(super) fn allow_downloads() -> bool {
        // SAFETY: plain integer arguments; the call only changes this
        // process's I/O policy and reports failure through its return value.
        let status = unsafe {
            setiopolicy_np(
                IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                IOPOL_SCOPE_PROCESS,
                IOPOL_MATERIALIZE_DATALESS_FILES_ON,
            )
        };
        status == 0
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub(super) fn allow_downloads() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn turns_the_policy_on_where_it_exists() {
        assert_eq!(super::allow_downloads(), cfg!(target_os = "macos"));
        // Idempotent: a second call is harmless.
        assert_eq!(super::allow_downloads(), cfg!(target_os = "macos"));
    }
}
