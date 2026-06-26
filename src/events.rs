use bevy::ecs::message::Message;
use objc2::rc::Retained;
use objc2_core_foundation::{
    CFRetained, CFRunLoop, CFRunLoopSource, CFRunLoopSourceContext, CGPoint, kCFRunLoopCommonModes,
};
use objc2_core_graphics::CGDirectDisplayID;
use std::ffi::c_void;
use std::fmt;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::commands::Command;
use crate::config::Config;
use crate::ecs::state::StateQueryKind;
use crate::errors::Result;
use crate::platform::{Modifiers, ProcessSerialNumber, WinID, WorkspaceId, WorkspaceObserver};
use crate::util::AXUIWrapper;

/// `Event` represents various system-level and application-specific occurrences that the window manager reacts to.
/// These events drive the core logic of the window manager, from window creation to display changes.
#[allow(dead_code)]
#[derive(Clone, Debug, Message)]
pub enum Event {
    /// Signals the application to exit.
    Exit,
    /// Indicates that the initial set of processes has been loaded.
    ProcessesLoaded,

    /// Announces the initialy loaded configuration
    InitialConfig(Config),
    /// Signals that the configuration should be reloaded.
    ConfigRefresh(notify::Event),

    /// An application has been launched.
    ApplicationLaunched {
        psn: ProcessSerialNumber,
        observer: Retained<WorkspaceObserver>,
    },

    /// An application has terminated.
    ApplicationTerminated { psn: ProcessSerialNumber },
    /// The frontmost application has switched.
    ApplicationFrontSwitched { psn: ProcessSerialNumber },
    /// The application has been activated.
    ApplicationActivated,
    /// The application has been deactivated.
    ApplicationDeactivated,
    /// An application has become visible.
    ApplicationVisible { pid: i32 },
    /// An application has become hidden.
    ApplicationHidden { pid: i32 },

    /// A window has been created.
    WindowCreated { element: CFRetained<AXUIWrapper> },
    /// A window has been destroyed.
    WindowDestroyed { window_id: WinID },
    /// A window has gained focus.
    WindowFocused { window_id: WinID },
    /// A window has been moved.
    WindowMoved { window_id: WinID },
    /// A window has been resized.
    WindowResized { window_id: WinID },
    /// A window has been minimized.
    WindowMinimized { window_id: WinID },
    /// A window has been de-minimized (restored).
    WindowDeminimized { window_id: WinID },
    /// A window's title has changed.
    WindowTitleChanged { window_id: WinID },

    /// A mouse down event has occurred.
    MouseDown {
        point: CGPoint,
        modifiers: Modifiers,
    },
    /// A mouse up event has occurred.
    MouseUp {
        point: CGPoint,
        modifiers: Modifiers,
    },
    /// A mouse drag event has occurred.
    MouseDragged {
        point: CGPoint,
        modifiers: Modifiers,
    },
    /// A mouse move event has occurred.
    MouseMoved {
        point: CGPoint,
        modifiers: Modifiers,
    },

    /// A swipe gesture has been detected.
    Swipe { delta: f64, fingers: usize },

    /// A vertical trackpad gesture (accumulates delta to threshold before firing).
    VerticalSwipe { delta: f64, fingers: usize },

    /// A single scroll wheel tick for vertical workspace switching (fires immediately).
    VerticalScrollTick { delta: f64 },

    /// A mouse scroll has been detected.
    Scroll { delta: f64 },

    /// Fingers have been placed on the touchpad.
    TouchpadDown,
    /// All fingers are up from the touchpad.
    TouchpadUp,

    /// A new space (virtual desktop) has been created.
    SpaceCreated { space_id: WorkspaceId },
    /// A space has been destroyed.
    SpaceDestroyed { space_id: WorkspaceId },
    /// The active space has changed.
    SpaceChanged,

    /// A new display has been added.
    DisplayAdded { display_id: CGDirectDisplayID },
    /// A display has been removed.
    DisplayRemoved { display_id: CGDirectDisplayID },
    /// A display has been moved.
    DisplayMoved { display_id: CGDirectDisplayID },
    /// A display has been resized.
    DisplayResized { display_id: CGDirectDisplayID },
    /// A display's configuration has changed.
    DisplayConfigured { display_id: CGDirectDisplayID },
    /// The overall display arrangement has changed.
    DisplayChanged,

    /// Mission Control: Show all windows.
    MissionControlShowAllWindows,
    /// Mission Control: Show frontmost application windows.
    MissionControlShowFrontWindows,
    /// Mission Control: Show desktop.
    MissionControlShowDesktop,
    /// Mission Control: Exit.
    MissionControlExit,

    /// Dock preferences have changed.
    DockDidChangePref { msg: String },
    /// The Dock has restarted.
    DockDidRestart { msg: String },

    /// A menu has been opened.
    MenuOpened { window_id: WinID },
    /// A menu has been closed.
    MenuClosed { window_id: WinID },
    /// The visibility of the menu bar has changed.
    MenuBarHiddenChanged { msg: String },
    /// The system has woken from sleep.
    SystemWoke { msg: String },

    /// The system appearance (Light/Dark mode) has changed.
    ThemeChanged,

    /// A command has been issued to the window manager.
    Command { command: Command },

    /// A structured state query has been issued by a socket client.
    StateQuery {
        kind: StateQueryKind,
        respond_to: Sender<String>,
    },

    /// A socket client has subscribed to line-delimited state events.
    StateSubscribe { stream: Arc<Mutex<UnixStream>> },
}

trait EventWaker: Send + Sync {
    fn wake(&self);
}

#[derive(Clone, Default)]
struct WakeCoalescer {
    pending: Arc<AtomicBool>,
}

impl WakeCoalescer {
    fn request_signal(&self) -> bool {
        !self.pending.swap(true, Ordering::SeqCst)
    }

    #[cfg(test)]
    fn complete(&self) {
        self.pending.store(false, Ordering::SeqCst);
    }
}

struct MainRunLoopWaker {
    run_loop: CFRetained<CFRunLoop>,
    source: CFRetained<CFRunLoopSource>,
    coalescer: WakeCoalescer,
}

// The source is registered on the main run loop during construction. Apple
// documents `CFRunLoopSourceSignal` and `CFRunLoopWakeUp` as callable from other
// threads; `wake` is the only cross-thread operation performed through this
// wrapper. Registration and invalidation happen while the owner lives on the
// main thread in normal Paneru startup/shutdown.
unsafe impl Send for MainRunLoopWaker {}
unsafe impl Sync for MainRunLoopWaker {}

impl MainRunLoopWaker {
    fn new() -> Option<Self> {
        let run_loop = CFRunLoop::main()?;
        let coalescer = WakeCoalescer::default();
        let mut context = CFRunLoopSourceContext {
            version: 0,
            info: Arc::as_ptr(&coalescer.pending).cast_mut().cast::<c_void>(),
            retain: Some(retain_atomic_bool),
            release: Some(release_atomic_bool),
            copyDescription: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform: Some(perform_wake_source),
        };
        let source = unsafe { CFRunLoopSource::new(None, 0, &raw mut context)? };
        CFRunLoop::add_source(&run_loop, Some(&source), unsafe { kCFRunLoopCommonModes });
        Some(Self {
            run_loop,
            source,
            coalescer,
        })
    }
}

impl Drop for MainRunLoopWaker {
    fn drop(&mut self) {
        self.source.invalidate();
        CFRunLoop::remove_source(&self.run_loop, Some(&self.source), unsafe {
            kCFRunLoopCommonModes
        });
    }
}

impl EventWaker for MainRunLoopWaker {
    fn wake(&self) {
        if self.coalescer.request_signal() {
            self.source.signal();
        }
        self.run_loop.wake_up();
    }
}

unsafe extern "C-unwind" fn retain_atomic_bool(info: *const c_void) -> *const c_void {
    unsafe { Arc::increment_strong_count(info.cast::<AtomicBool>()) };
    info
}

unsafe extern "C-unwind" fn release_atomic_bool(info: *const c_void) {
    unsafe { Arc::decrement_strong_count(info.cast::<AtomicBool>()) };
}

unsafe extern "C-unwind" fn perform_wake_source(info: *mut c_void) {
    let pending = unsafe { &*info.cast::<AtomicBool>() };
    pending.store(false, Ordering::SeqCst);
}

#[derive(Default)]
struct NoopWaker;

impl EventWaker for NoopWaker {
    fn wake(&self) {}
}

/// Wakeable receive side for Paneru's internal event queue.
///
/// Every successful [`EventSender::send`] signals the main Cocoa run loop after
/// the event is queued, so longer idle sleeps cannot strand commands, queries,
/// or macOS callback events until a polling timeout expires.
pub struct WakeableEventQueue {
    rx: Receiver<Event>,
}

impl WakeableEventQueue {
    pub fn recv(&self) -> std::result::Result<Event, std::sync::mpsc::RecvError> {
        self.rx.recv()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> std::result::Result<Event, RecvTimeoutError> {
        self.rx.recv_timeout(timeout)
    }
}

/// `EventSender` is a wakeable wrapper around a `std::sync::mpsc::Sender` for
/// `Event`s. It provides a convenient way to send events to the main event loop
/// from various parts of the application.
#[derive(Clone)]
pub struct EventSender {
    tx: Sender<Event>,
    waker: Arc<dyn EventWaker>,
}

impl fmt::Debug for EventSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventSender").finish_non_exhaustive()
    }
}

impl EventSender {
    /// Creates a new `EventSender` and its corresponding wakeable receiver.
    /// This function initializes an MPSC channel and captures the main Cocoa run
    /// loop as the wake target when available.
    ///
    /// # Returns
    ///
    /// A tuple containing the `EventSender` and receiver for the created queue.
    pub fn new() -> (Self, WakeableEventQueue) {
        let waker: Arc<dyn EventWaker> = MainRunLoopWaker::new().map_or_else(
            || Arc::new(NoopWaker) as Arc<dyn EventWaker>,
            |waker| Arc::new(waker) as Arc<dyn EventWaker>,
        );
        Self::new_with_waker(waker)
    }

    fn new_with_waker(waker: Arc<dyn EventWaker>) -> (Self, WakeableEventQueue) {
        let (tx, rx) = channel::<Event>();
        (Self { tx, waker }, WakeableEventQueue { rx })
    }

    /// Sends an `Event` through the internal channel and wakes the main loop.
    ///
    /// The event is queued before the wake signal is sent. If the receiver has
    /// already shut down, the send error is returned and no wake is attempted.
    ///
    /// # Arguments
    ///
    /// * `event` - The `Event` to send.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the event is sent successfully, otherwise `Err(Error)` if the receiver has disconnected.
    pub fn send(&self, event: Event) -> Result<()> {
        self.tx.send(event)?;
        self.waker.wake();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    #[derive(Default)]
    struct CountingWaker {
        count: AtomicUsize,
    }

    impl EventWaker for CountingWaker {
        fn wake(&self) {
            self.count.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn send_queues_event_before_waking() {
        let waker = Arc::new(CountingWaker::default());
        let (sender, receiver) = EventSender::new_with_waker(waker.clone());

        sender.send(Event::ProcessesLoaded).unwrap();

        assert!(matches!(
            receiver.recv_timeout(Duration::from_millis(1)).unwrap(),
            Event::ProcessesLoaded
        ));
        assert_eq!(waker.count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn send_from_background_thread_wakes_main_loop() {
        let waker = Arc::new(CountingWaker::default());
        let (sender, receiver) = EventSender::new_with_waker(waker.clone());
        let sender = sender.clone();

        thread::spawn(move || sender.send(Event::ProcessesLoaded))
            .join()
            .unwrap()
            .unwrap();

        assert!(matches!(
            receiver.recv_timeout(Duration::from_millis(1)).unwrap(),
            Event::ProcessesLoaded
        ));
        assert_eq!(waker.count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn disconnected_receiver_returns_error_without_waking() {
        let waker = Arc::new(CountingWaker::default());
        let (sender, receiver) = EventSender::new_with_waker(waker.clone());
        drop(receiver);

        assert!(sender.send(Event::ProcessesLoaded).is_err());
        assert_eq!(waker.count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn burst_sends_do_not_drop_events() {
        let waker = Arc::new(CountingWaker::default());
        let (sender, receiver) = EventSender::new_with_waker(waker.clone());

        for _ in 0..128 {
            sender.send(Event::ProcessesLoaded).unwrap();
        }

        for _ in 0..128 {
            assert!(matches!(
                receiver.recv_timeout(Duration::from_millis(1)).unwrap(),
                Event::ProcessesLoaded
            ));
        }
        assert_eq!(waker.count.load(Ordering::SeqCst), 128);
    }

    #[test]
    fn wake_coalescer_signals_once_until_source_performs() {
        let coalescer = WakeCoalescer::default();

        assert!(coalescer.request_signal());
        assert!(!coalescer.request_signal());
        coalescer.complete();
        assert!(coalescer.request_signal());
    }
}
