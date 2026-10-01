use crate::Pinch;
use block2::RcBlock;
use objc2::{MainThreadMarker, rc::Retained, runtime::AnyObject};
use objc2_app_kit::{NSEvent, NSEventMask, NSView};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{cell::RefCell, ptr::NonNull, sync::Arc};

thread_local! {
    // Created and destroyed only on AppKit's main thread. AppKit owns the copied callback block.
    static MONITOR: RefCell<Option<Monitor>> = const { RefCell::new(None) };
}

struct Monitor(Retained<AnyObject>);

impl Drop for Monitor {
    fn drop(&mut self) {
        // SAFETY: this is exactly the token returned by addLocalMonitor, retained until removal;
        // the thread-local slot is accessed only after obtaining MainThreadMarker.
        unsafe { NSEvent::removeMonitor(&self.0) };
    }
}

pub fn install_pinch_handler(
    window: &dyn HasWindowHandle,
    handler: Arc<dyn Fn(Pinch) + Send + Sync>,
) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("Trackpad input must start on the main thread")?;
    let handle = window.window_handle().map_err(|error| error.to_string())?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err("Trackpad input needs an AppKit window".into());
    };
    // SAFETY: HasWindowHandle guarantees a valid NSView for the duration of this borrow. We are
    // on the main thread, copy only its window number, and retain no native view pointer.
    let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
    let window_number = view
        .window()
        .ok_or("Trackpad view has no window")?
        .windowNumber();
    let block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit invokes this local monitor on the main thread with a live NSEvent.
        // The borrow ends within the callback, and we return that same, unmodified event pointer.
        let borrowed = unsafe { event.as_ref() };
        // A local monitor also sees native file dialogs. They must never zoom the editor, even
        // if a dialog's nested event loop delays this callback's wake until after it closes.
        if borrowed.windowNumber() != window_number {
            return event.as_ptr();
        }
        if let Some(window) = borrowed.window(mtm)
            && let Some(view) = window.contentView()
        {
            let point = view.convertPoint_fromView(borrowed.locationInWindow(), None);
            let bounds = view.bounds();
            let y = if view.isFlipped() {
                point.y - bounds.origin.y
            } else {
                bounds.origin.y + bounds.size.height - point.y
            };
            handler(Pinch {
                delta: borrowed.magnification(),
                x: point.x - bounds.origin.x,
                y,
            });
        }
        event.as_ptr()
    });
    // SAFETY: every callback returns the original live event pointer; AppKit copies the block.
    let monitor = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::Magnify, &block)
    }
    .ok_or("AppKit could not install trackpad input")?;
    MONITOR.with(|slot| *slot.borrow_mut() = Some(Monitor(monitor)));
    Ok(())
}
