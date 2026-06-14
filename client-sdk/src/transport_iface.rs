//! The client↔V-App transport seam, independent of any concrete transport.
//!
//! Kept free of the native engine/tokio so it compiles everywhere — in particular on
//! `wasm32`, where a V-App and its client run co-resident in one module (see
//! [`WasmAppTransport`]). The native transports (TCP/HID/VM) live in `vanadium_client`.

use async_trait::async_trait;

/// Represents errors that can occur during the execution of a V-App.
#[derive(Debug)]
pub enum VAppExecutionError {
    /// Indicates that no V-App is currently running.
    VAppNotRunning,
    /// Indicates that the V-App has panicked with the specific message.
    AppPanicked(String),
    /// Indicates that the V-App has exited with the specific status code.
    /// Useful to handle a graceful exit of the V-App.
    AppExited(i32),
    /// Any other error.
    Other(Box<dyn std::error::Error + Send + Sync>),
}

impl std::fmt::Display for VAppExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            VAppExecutionError::VAppNotRunning => write!(f, "No V-App is currently running"),
            VAppExecutionError::AppPanicked(msg) => write!(f, "V-App panicked: {}", msg),
            VAppExecutionError::AppExited(code) => write!(f, "V-App exited with status {}", code),
            VAppExecutionError::Other(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for VAppExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            VAppExecutionError::VAppNotRunning => None,
            VAppExecutionError::AppPanicked(_) => None,
            VAppExecutionError::AppExited(_) => None,
            VAppExecutionError::Other(e) => Some(&**e),
        }
    }
}

/// A trait representing an application that can send messages asynchronously.
///
/// This trait defines the behavior for sending messages to an application and
/// receiving responses.
///
/// `?Send`: a co-resident wasm app's handler future is not `Send`, and the native
/// transports only ever `.await` `send_message` inline (never spawn it), so dropping the
/// `Send` bound on the returned future costs the native side nothing.
#[async_trait(?Send)]
pub trait VAppTransport {
    /// Sends a message to the app and returns the response asynchronously.
    ///
    /// # Parameters
    ///
    /// - `msg`: A `&[u8]` containing the message to be sent.
    ///
    /// # Returns
    ///
    /// A `Result` containing the response message as a `Vec<u8>` if the operation is successful,
    /// or a `VAppExecutionError` if an error occurs.
    async fn send_message(&mut self, msg: &[u8]) -> Result<Vec<u8>, VAppExecutionError>;
}

/// A [`VAppTransport`] for a V-App that is co-resident in the same wasm module as the
/// client (architecture A): `send_message` runs the app's handler directly — no socket —
/// and `.await`s it, so a handler that waits for user input suspends back to the JS
/// step-driver instead of blocking.
#[cfg(feature = "wasm")]
pub struct WasmAppTransport<S = ()> {
    // Boxed so the app keeps a stable address: the demo grabs `app_ptr()` to pump the app's
    // idle/dashboard UX (architecture A) while no command is in flight, and that pointer must
    // survive moving the transport into a `Box<dyn VAppTransport>`.
    app: Box<app_sdk::App<S>>,
}

#[cfg(feature = "wasm")]
impl<S: Default> WasmAppTransport<S> {
    /// Builds the transport from an `AppBuilder` (the co-resident V-App).
    pub fn new(builder: app_sdk::AppBuilder<S>) -> Self {
        Self {
            app: Box::new(builder.build_wasm()),
        }
    }

    /// A stable pointer to the co-resident app, so the runtime can pump its idle/dashboard UX
    /// (`App::idle_ux_step`) between commands. Sound only single-threaded and never while a
    /// command is in flight (the transport's `send_message` is then borrowing the same app);
    /// the caller upholds that, exactly like the step drivers do.
    pub fn app_ptr(&mut self) -> *mut app_sdk::App<S> {
        &mut *self.app
    }
}

#[cfg(feature = "wasm")]
#[async_trait(?Send)]
impl<S: Default> VAppTransport for WasmAppTransport<S> {
    async fn send_message(&mut self, msg: &[u8]) -> Result<Vec<u8>, VAppExecutionError> {
        Ok(self.app.dispatch(msg).await)
    }
}
