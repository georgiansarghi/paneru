use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use objc2_core_foundation::{
    CFAbsoluteTimeGetCurrent, CFRetained, CFRunLoop, CFRunLoopTimer, CFRunLoopTimerContext,
    kCFRunLoopCommonModes, kCFRunLoopDefaultMode,
};
use tracing::trace;

pub(super) struct MainRunLoopWaiter {
    deadline_timer: MainRunLoopDeadlineTimer,
}

impl MainRunLoopWaiter {
    pub(super) fn new() -> Option<Self> {
        Some(Self {
            deadline_timer: MainRunLoopDeadlineTimer::new()?,
        })
    }

    pub(super) fn wait(&mut self, timeout: Duration) -> RunnerWaitOutcome {
        self.deadline_timer.schedule_after(timeout);
        trace!(
            timeout_ms = timeout.as_millis(),
            "waiting on runner-owned CFRunLoop source/timer set"
        );
        CFRunLoop::run_in_mode(
            unsafe { kCFRunLoopDefaultMode },
            timeout.as_secs_f64(),
            true,
        );
        RunnerWaitOutcome {
            deadline_fired: self.deadline_timer.take_fired(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct RunnerWaitOutcome {
    pub(super) deadline_fired: bool,
}

struct MainRunLoopDeadlineTimer {
    run_loop: CFRetained<CFRunLoop>,
    timer: CFRetained<CFRunLoopTimer>,
    fired: Arc<AtomicBool>,
}

impl MainRunLoopDeadlineTimer {
    fn new() -> Option<Self> {
        let run_loop = CFRunLoop::main()?;
        let fired = Arc::new(AtomicBool::new(false));
        let mut context = CFRunLoopTimerContext {
            version: 0,
            info: Arc::as_ptr(&fired).cast_mut().cast::<c_void>(),
            retain: Some(retain_atomic_bool),
            release: Some(release_atomic_bool),
            copyDescription: None,
        };
        let timer = unsafe {
            CFRunLoopTimer::new(
                None,
                far_future_fire_date(),
                0.0,
                0,
                0,
                Some(deadline_timer_fired),
                &raw mut context,
            )?
        };
        CFRunLoop::add_timer(&run_loop, Some(&timer), unsafe { kCFRunLoopCommonModes });
        Some(Self {
            run_loop,
            timer,
            fired,
        })
    }

    fn schedule_after(&mut self, duration: Duration) {
        self.fired.store(false, Ordering::SeqCst);
        self.timer
            .set_next_fire_date(CFAbsoluteTimeGetCurrent() + duration.as_secs_f64());
    }

    fn take_fired(&self) -> bool {
        self.fired.swap(false, Ordering::SeqCst)
    }
}

impl Drop for MainRunLoopDeadlineTimer {
    fn drop(&mut self) {
        self.timer.invalidate();
        CFRunLoop::remove_timer(&self.run_loop, Some(&self.timer), unsafe {
            kCFRunLoopCommonModes
        });
    }
}

fn far_future_fire_date() -> f64 {
    CFAbsoluteTimeGetCurrent() + 60.0 * 60.0 * 24.0 * 365.0
}

unsafe extern "C-unwind" fn retain_atomic_bool(info: *const c_void) -> *const c_void {
    unsafe { Arc::increment_strong_count(info.cast::<AtomicBool>()) };
    info
}

unsafe extern "C-unwind" fn release_atomic_bool(info: *const c_void) {
    unsafe { Arc::decrement_strong_count(info.cast::<AtomicBool>()) };
}

unsafe extern "C-unwind" fn deadline_timer_fired(_timer: *mut CFRunLoopTimer, info: *mut c_void) {
    let fired = unsafe { &*info.cast::<AtomicBool>() };
    fired.store(true, Ordering::SeqCst);
}
