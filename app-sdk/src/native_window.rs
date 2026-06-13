//! Native OS window for the simulator (opt-in `native-window` feature).
//!
//! Opens a desktop window with an embedded webview pointed at the local viewer URL
//! served by [`webui`](crate::ecalls_native), so the simulated device shows up as a
//! normal window that opens automatically — no browser step. The window must own the
//! main thread (a hard requirement on macOS), which [`crate::App::run`] arranges by
//! moving the app loop to a worker thread.
//!
//! On Linux this uses WebKitGTK (needs `libwebkit2gtk-4.1-dev`); macOS and Windows use
//! the system webview, so no extra system package is required there.

use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};
use wry::WebViewBuilder;

use common::ecall_constants::{display_unpack_pair, DEVICE_PROPERTY_SCREEN_SIZE};

/// Starts the viewer server (if enabled) and returns its URL, or `None` when the viewer
/// is disabled (headless / tests) or could not bind a port.
pub(crate) fn should_open() -> Option<String> {
    crate::ecalls_native::webui_url()
}

/// Runs the native window's event loop on the calling (main) thread. Never returns:
/// closing the window exits the process, which also stops the app worker thread.
pub(crate) fn run(url: String) -> ! {
    let (win_w, win_h) = window_size();

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("Vanadium V-App")
        .with_inner_size(LogicalSize::new(win_w, win_h))
        .build(&event_loop)
        .expect("failed to create the native window");

    // The webview just loads the same local viewer the browser would; all rendering and
    // input handling live in the page (app-sdk/src/webui/viewer.html).
    //
    // On Linux, attach the webview to the window's GTK container rather than going through
    // the raw window handle: the raw-handle path is backend-dependent (it fails with
    // `UnsupportedWindowHandle` under Wayland and some GTK setups), whereas the GTK path
    // works on both X11 and Wayland. macOS/Windows use the portable raw-handle path.
    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window
            .default_vbox()
            .expect("native window has no GTK container");
        WebViewBuilder::new_gtk(vbox).with_url(&url).build()
    };
    #[cfg(not(target_os = "linux"))]
    let webview = WebViewBuilder::new(&window).with_url(&url).build();

    let _webview = webview.expect("failed to create the webview");

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            std::process::exit(0);
        }
    });
}

// A window that comfortably fits the page's scaled canvas plus its title row and the
// Stop/Quit buttons, derived from the emulated device's screen size.
fn window_size() -> (f64, f64) {
    let (w, h) = display_unpack_pair(crate::get_device_property(DEVICE_PROPERTY_SCREEN_SIZE));
    let (w, h) = (w.max(1) as f64, h.max(1) as f64);
    // The page scales small panels up toward ~480px wide; mirror that so Nano windows
    // aren't tiny.
    let scale = (480.0 / w).floor().max(1.0);
    ((w * scale).max(360.0) + 40.0, h * scale + 170.0)
}
