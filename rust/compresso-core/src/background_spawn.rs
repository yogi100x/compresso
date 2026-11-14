//! Console-window suppression policy for background child launches.
//!
//! On Windows a console-subsystem child started by a windowless or GUI parent
//! is given a brand-new console window, which flashes on screen for every
//! probe, version check, or server launch. Passing `CREATE_NO_WINDOW` in the
//! creation flags suppresses it. Other platforms have no equivalent flag.
//!
//! Compresso's local crates are deliberately free of child-launch capability
//! (see the capability matrix in `docs/plan/01-stack-and-workspace.md`), so
//! this module holds only the policy, not a launcher. Every launch site, in a
//! launch-capable crate or an integration test, routes its command builder
//! through [`suppress_console_window`]:
//!
//! ```ignore
//! #[cfg(windows)]
//! use std::os::windows::process::CommandExt as _;
//!
//! compresso_core::suppress_console_window(|flags| {
//!     #[cfg(windows)]
//!     command.creation_flags(flags);
//!     #[cfg(not(windows))]
//!     let _ = flags;
//! });
//! ```
//!
//! The standard command builder replaces, rather than merges, its creation
//! flags; a launch site that needs additional flags must OR them with the
//! value it is handed.

/// Windows `CREATE_NO_WINDOW` creation flag.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Creation flags every background child launch must set on this platform.
///
/// Returns `None` where the platform has no creation flags to set.
pub const fn background_creation_flags() -> Option<u32> {
    background_creation_flags_for(cfg!(windows))
}

const fn background_creation_flags_for(windows: bool) -> Option<u32> {
    if windows {
        Some(CREATE_NO_WINDOW)
    } else {
        None
    }
}

/// Applies the background-launch creation flags through `set_creation_flags`.
///
/// The setter is invoked exactly once on Windows and never elsewhere. Returns
/// whether it was invoked, so a launch site can assert its own wiring.
pub fn suppress_console_window(set_creation_flags: impl FnOnce(u32)) -> bool {
    match background_creation_flags() {
        Some(flags) => {
            set_creation_flags(flags);
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_windows_policy_is_create_no_window_and_other_platforms_have_none() {
        assert_eq!(background_creation_flags_for(true), Some(0x0800_0000));
        assert_eq!(background_creation_flags_for(false), None);
    }

    #[test]
    fn test_helper_applies_exactly_the_platform_policy_to_the_builder() {
        let mut recorded = Vec::new();

        let applied = suppress_console_window(|flags| recorded.push(flags));

        assert_eq!(applied, cfg!(windows));
        assert_eq!(
            recorded,
            background_creation_flags().into_iter().collect::<Vec<_>>()
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_windows_builder_receives_create_no_window() {
        let mut recorded = None;

        assert!(suppress_console_window(|flags| recorded = Some(flags)));

        assert_eq!(recorded, Some(CREATE_NO_WINDOW));
    }

    #[cfg(not(windows))]
    #[test]
    fn test_non_windows_builder_is_left_untouched() {
        let mut recorded = None;

        assert!(!suppress_console_window(|flags| recorded = Some(flags)));

        assert_eq!(recorded, None);
    }
}
