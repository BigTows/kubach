//! The macOS Dock icon of a binary run outside an `.app` bundle
//! (`cargo run`): a bundle shows its own `kubach.icns`. Windows takes the
//! icon from the resource `build.rs` embeds, Linux from the `.desktop` file.

#[cfg(target_os = "macos")]
pub fn set_dock_icon() {
    use objc2::{AllocAnyThread as _, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    const PNG: &[u8] = include_bytes!("../assets/icon/kubach-512.png");

    let bundled = std::env::current_exe()
        .is_ok_and(|exe| exe.to_string_lossy().contains(".app/Contents/MacOS/"));
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    if bundled {
        return;
    }
    let Some(image) = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(PNG)) else {
        return;
    };
    // SAFETY: a valid image, set on the main thread.
    unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
}

#[cfg(not(target_os = "macos"))]
pub fn set_dock_icon() {}
