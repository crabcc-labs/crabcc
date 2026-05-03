//! Native-OS hooks. Today this is a single function — `set_dock_badge`
//! — that paints a label on the macOS dock tile when the running-agent
//! count is non-zero.
//!
//! Cross-platform contract: `set_dock_badge(Some("3"))` shows the
//! number, `set_dock_badge(None)` clears it. On non-macOS platforms
//! the call is a compile-time no-op so the rest of the app reads
//! identically across targets.
//!
//! The macOS implementation calls `[[NSApp dockTile] setBadgeLabel:]`
//! via `objc2`. AppKit's documented contract is "must be called on the
//! main thread"; gpui's render path runs on the main thread already,
//! so we don't gate this behind a runtime check.

#[cfg(target_os = "macos")]
mod imp {
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSString;

    /// Set or clear the dock-tile badge label. Pass `None` to remove
    /// the badge entirely; pass `Some(text)` to set it (typically a
    /// 1-3 character count). AppKit accepts arbitrary strings but
    /// truncates anything longer than ~6 chars to fit the badge oval.
    pub fn set_dock_badge(label: Option<&str>) {
        autoreleasepool(|_| {
            // Both this call and the dock-tile mutation must run on
            // the main thread; gpui guarantees that from the render
            // path. Acquire a `MainThreadMarker` to prove it to the
            // type-checker — `expect` rather than silent fallback so
            // a future off-thread caller fails loudly.
            let mtm = objc2_foundation::MainThreadMarker::new()
                .expect("native::set_dock_badge called off the main thread");
            let app = NSApplication::sharedApplication(mtm);
            let tile = app.dockTile();
            // `setBadgeLabel:` takes either an NSString or nil. The
            // safe binding accepts `Option<&NSString>`. Empty-string
            // also clears the badge per AppKit docs, but passing nil
            // is the canonical "no badge" form.
            let nsstr: Option<objc2::rc::Retained<NSString>> = label.map(NSString::from_str);
            tile.setBadgeLabel(nsstr.as_deref());
        });
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    /// No-op stub on non-macOS platforms. Lets the rest of the app
    /// call `set_dock_badge` unconditionally without `cfg` blocks at
    /// every call site.
    pub fn set_dock_badge(_label: Option<&str>) {}
}

pub use imp::set_dock_badge;
