//! Native-OS hooks. Two surfaces share the same data today (the
//! running-agents count): the macOS dock-tile badge and the menu-bar
//! status item.
//!
//! Cross-platform contract:
//!   * `set_dock_badge(Some(text))` paints the dock-tile badge; `None` clears.
//!   * `set_status_item(Some(text))` shows / updates the menu-bar status
//!     item with `text` as its button title; `None` hides it.
//!
//! On non-macOS platforms both calls are compile-time no-ops so the
//! rest of the app reads identically across targets.
//!
//! AppKit's documented contract is "must be called on the main
//! thread"; gpui's render path runs on the main thread already, so we
//! prove that to the type-checker via `MainThreadMarker` and `expect`
//! a future off-thread caller into a panic.

#[cfg(target_os = "macos")]
mod imp {
    use std::cell::RefCell;

    use objc2::rc::{autoreleasepool, Retained};
    use objc2_app_kit::{NSApplication, NSStatusBar, NSStatusItem, NSVariableStatusItemLength};
    use objc2_foundation::NSString;

    /// Set or clear the dock-tile badge label. Pass `None` to remove
    /// the badge entirely; pass `Some(text)` to set it (typically a
    /// 1-3 character count). AppKit accepts arbitrary strings but
    /// truncates anything longer than ~6 chars to fit the badge oval.
    pub fn set_dock_badge(label: Option<&str>) {
        autoreleasepool(|_| {
            let mtm = objc2_foundation::MainThreadMarker::new()
                .expect("native::set_dock_badge called off the main thread");
            let app = NSApplication::sharedApplication(mtm);
            let tile = app.dockTile();
            let nsstr: Option<Retained<NSString>> = label.map(NSString::from_str);
            tile.setBadgeLabel(nsstr.as_deref());
        });
    }

    thread_local! {
        /// Cached `NSStatusItem` so we don't allocate a new menu-bar
        /// icon on every render. Created lazily on the first call to
        /// `set_status_item(Some(_))`. `RefCell` rather than
        /// `OnceCell` because the cell needs to support the
        /// "create-on-demand, mutate-on-update" pattern; only the
        /// gpui main thread touches it (thread-local + main-thread
        /// guarantee), so the borrow_mut is contention-free.
        static STATUS_ITEM: RefCell<Option<Retained<NSStatusItem>>> = const { RefCell::new(None) };
    }

    /// Set or clear the menu-bar status item. The first `Some(_)`
    /// call lazily allocates an `NSStatusItem` of variable length
    /// from the system status bar; subsequent calls just update its
    /// button title. Passing `None` flips the item's `visible`
    /// property off — this is faster than tearing down and
    /// recreating the item, and keeps any user-clicked menu state
    /// intact for when the count comes back.
    pub fn set_status_item(label: Option<&str>) {
        autoreleasepool(|_| {
            let mtm = objc2_foundation::MainThreadMarker::new()
                .expect("native::set_status_item called off the main thread");

            STATUS_ITEM.with(|cell| {
                let mut slot = cell.borrow_mut();
                match label {
                    Some(text) => {
                        // Lazily create the item — first non-empty
                        // call wins. `NSVariableStatusItemLength`
                        // sizes the item to its title content rather
                        // than reserving a fixed slot.
                        let item = slot.get_or_insert_with(|| {
                            let bar = NSStatusBar::systemStatusBar();
                            bar.statusItemWithLength(NSVariableStatusItemLength)
                        });
                        // Modern API path: title goes through the
                        // item's `button`, not the deprecated
                        // `setTitle:` on the item itself. The button
                        // is `Option<_>` because AppKit could
                        // theoretically return a custom view item;
                        // for variable-length text items it's always
                        // present.
                        if let Some(button) = item.button(mtm) {
                            let title = NSString::from_str(text);
                            button.setTitle(&title);
                        }
                        item.setVisible(true);
                    }
                    None => {
                        // Hide rather than tear down — fewer AppKit
                        // round-trips when the count toggles, and
                        // future revs can re-show without
                        // re-allocating.
                        if let Some(item) = slot.as_ref() {
                            item.setVisible(false);
                        }
                    }
                }
            });
        });
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    /// No-op stub on non-macOS platforms. Lets the rest of the app
    /// call `set_dock_badge` / `set_status_item` unconditionally
    /// without `cfg` blocks at every call site.
    pub fn set_dock_badge(_label: Option<&str>) {}
    pub fn set_status_item(_label: Option<&str>) {}
}

pub use imp::{set_dock_badge, set_status_item};
