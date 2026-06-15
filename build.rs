// SPDX-License-Identifier: GPL-3.0-or-later
//! Embed the application icon (assets/rust-terminal.ico) into the executable as
//! resource id 101, so Explorer/taskbar/Alt-Tab show it and the window can load
//! it as its class icon.

fn main() {
    #[cfg(target_os = "windows")]
    {
        println!("cargo:rerun-if-changed=assets/rust-terminal.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon_with_id("assets/rust-terminal.ico", "101");
        // Best-effort: if the resource compiler isn't available, build without
        // the icon rather than failing.
        let _ = res.compile();
    }
}
