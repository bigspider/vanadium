//! Native OS window for the simulator (opt-in `native-window` feature).
//!
//! Opens a desktop window with an embedded webview pointed at the local viewer URL
//! served by [`webui`](crate::ecalls_native), so the simulated device shows up as a
//! normal window — automatically, for *any* V-App. The window is launched by the display
//! backend on the first frame (see `webui::ensure_started`), not by `App::run`, so it
//! does not matter how a V-App structures its `main` loop.
//!
//! On Linux and Windows the windowing event loop is allowed to run off the main thread
//! (`with_any_thread`), so we host it on a dedicated background thread and leave the app's
//! main thread untouched. On Linux this uses WebKitGTK (needs `libwebkit2gtk-4.1-dev`).
//!
//! macOS requires the windowing event loop to own the *main* thread, which a library
//! spawned from an arbitrary V-App's `main` cannot claim — so there we fall back to the
//! browser viewer (the URL is printed at startup).

#[cfg(any(target_os = "linux", target_os = "windows"))]
use common::ecall_constants::{display_unpack_pair, DEVICE_PROPERTY_SCREEN_SIZE};

/// Spawns the native window for `url` (the local viewer). Returns immediately; the window
/// runs on its own thread. A no-op-with-note on platforms where the event loop cannot run
/// off the main thread.
pub(crate) fn spawn(url: String) {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        let _ = std::thread::Builder::new()
            .name("vapp-window".into())
            .spawn(move || run_event_loop(url));
    }
    // Other platforms (notably macOS) require the event loop to own the main thread,
    // which a library spawned from an arbitrary V-App's `main` can't claim, so the
    // browser viewer (URL printed at startup) is the entry point there.
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = url;
    }
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn run_event_loop(url: String) -> ! {
    use tao::{
        dpi::LogicalSize,
        event::{Event, WindowEvent},
        event_loop::{ControlFlow, EventLoopBuilder},
        window::WindowBuilder,
    };
    use wry::WebViewBuilder;

    let (win_w, win_h) = window_size();

    // Build the event loop on this (non-main) thread — allowed on Linux/Windows.
    let event_loop = {
        let mut builder = EventLoopBuilder::new();
        #[cfg(target_os = "linux")]
        {
            use tao::platform::unix::EventLoopBuilderExtUnix;
            builder.with_any_thread(true);
        }
        #[cfg(target_os = "windows")]
        {
            use tao::platform::windows::EventLoopBuilderExtWindows;
            builder.with_any_thread(true);
        }
        builder.build()
    };

    let window = WindowBuilder::new()
        .with_title("Vanadium V-App")
        .with_inner_size(LogicalSize::new(win_w, win_h))
        .build(&event_loop)
        .expect("failed to create the native window");

    // The webview just loads the same local viewer the browser would; all rendering and
    // input handling live in the page (app-sdk/src/webui/viewer.html).
    #[cfg(target_os = "linux")]
    let webview = {
        // Attach to the window's GTK container rather than going through the raw window
        // handle: the raw-handle path is backend-dependent (it fails with
        // `UnsupportedWindowHandle` under Wayland and some GTK setups), whereas the GTK
        // path works on both X11 and Wayland.
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
            // Closing the window ends the whole V-App.
            std::process::exit(0);
        }
    });
}

// A window that comfortably fits the page's scaled canvas plus its title row and the
// Stop/Quit buttons, derived from the emulated device's screen size.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn window_size() -> (f64, f64) {
    let (w, h) = display_unpack_pair(crate::get_device_property(DEVICE_PROPERTY_SCREEN_SIZE));
    let (w, h) = (w.max(1) as f64, h.max(1) as f64);
    // The page scales small panels up toward ~480px wide; mirror that so Nano windows
    // aren't tiny.
    let scale = (480.0 / w).floor().max(1.0);
    ((w * scale).max(360.0) + 40.0, h * scale + 170.0)
}
