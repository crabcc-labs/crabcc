/// Set (or clear) the macOS Dock tile badge.
///
/// * `label` — `Some("3")` displays the number; `None` clears the badge.
///
/// This call crosses the AppKit boundary and should be guarded by a
/// sentinel so it is only invoked when the value actually changes.
pub fn set_dock_badge(label: Option<&str>) {
    #[cfg(target_os = "macos")]
    {
        use objc::runtime::Object;
        use objc::*;

        unsafe {
            let app: *mut Object =
                msg_send![class!(NSApplication), sharedApplication];
            let dock_tile: *mut Object = msg_send![app, dockTile];

            let badge: *mut Object = match label {
                Some(s) => {
                    let bytes = s.as_bytes();
                    // NSUTF8StringEncoding = 4
                    msg_send![
                        class!(NSString),
                        stringWithBytes: bytes.as_ptr()
                        length: bytes.len()
                        encoding: 4u64
                    ]
                }
                None => std::ptr::null_mut(),
            };

            let _: () = msg_send![dock_tile, setBadgeLabel: badge];
            let _: () = msg_send![dock_tile, display];
        }
    }

    // Suppress unused-variable warning on non-macOS targets.
    #[cfg(not(target_os = "macos"))]
    let _ = label;
}
