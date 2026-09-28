//! Sparkle auto-updates. Compiled in only when `app/vendor/Sparkle.framework`
//! was present at build time (see `build.rs`); otherwise `start` and
//! `check_for_updates` are no-ops so the rest of the app doesn't need `#[cfg]`s.
//!
//! Setup: `scripts/fetch-sparkle.sh`, then see https://sparkle-project.org/documentation/ for signing
//! keys and hosting the appcast that `SUFeedURL` (Info-sparkle.plist) points at.

use std::ffi::c_void;

/// Opaque handle to the running `SPUStandardUpdaterController`; pass to
/// [`check_for_updates`]. Must not outlive the app (it's kept in a `Global`, never
/// dropped, matching Sparkle's own expectation that the controller lives forever).
pub type Controller = *mut c_void;

#[cfg(sparkle)]
// The `objc` crate's `msg_send!`/`class!` macros check `cfg(cargo-clippy)` internally.
#[allow(unexpected_cfgs)]
mod sparkle {
    use super::Controller;
    use objc::runtime::{Object, YES};
    use objc::{class, msg_send, sel, sel_impl};
    use std::ptr;

    pub fn start() -> Controller {
        // Don't spin up a real Cocoa updater against a bare test binary (no bundled
        // Info.plist for it to read); mirrors `recent_file()`'s test guard in main.rs.
        if cfg!(test) {
            return std::ptr::null_mut();
        }
        unsafe {
            let controller: *mut Object = msg_send![class!(SPUStandardUpdaterController), alloc];
            let controller: *mut Object = msg_send![controller,
                initWithStartingUpdater: YES
                updaterDelegate: ptr::null_mut::<Object>()
                userDriverDelegate: ptr::null_mut::<Object>()];
            controller as Controller
        }
    }

    pub fn check_for_updates(controller: Controller) {
        if controller.is_null() {
            return;
        }
        unsafe {
            let controller = controller as *mut Object;
            let _: () = msg_send![controller, checkForUpdates: ptr::null_mut::<Object>()];
        }
    }
}

#[cfg(not(sparkle))]
mod sparkle {
    use super::Controller;

    pub fn start() -> Controller {
        std::ptr::null_mut()
    }

    pub fn check_for_updates(_controller: Controller) {}
}

pub use sparkle::{check_for_updates, start};
