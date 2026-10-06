//! AppKit's `visible` KVO and lifecycle notifications, independent of the framework delegate.

use crate::visibility::Delivery;
use crate::{EvidenceVisibility, Visibility};
use block2::RcBlock;
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject},
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDidHideNotification,
    NSApplicationDidUnhideNotification, NSView, NSWindow, NSWindowDidDeminiaturizeNotification,
    NSWindowDidMiniaturizeNotification, NSWindowWillCloseNotification,
};
use objc2_foundation::{
    NSDictionary, NSKeyValueObservingOptions, NSNotification, NSNotificationCenter, NSObject,
    NSObjectNSKeyValueObserverRegistration, NSObjectProtocol, NSString, ns_string,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{cell::RefCell, ffi::c_void, ptr::NonNull, sync::Arc};

type Handler = Arc<dyn Fn(Visibility) + Send + Sync>;

thread_local! {
    // The editor owns one window. Retention and observer count stay constant across replacement.
    static MONITOR: RefCell<Option<Monitor>> = const { RefCell::new(None) };
}

struct ObserverState {
    window: Retained<NSWindow>,
    delivery: Delivery,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; ivars are main-thread-only.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ObserverState]
    struct VisibilityObserver;

    unsafe impl NSObjectProtocol for VisibilityObserver {}

    impl VisibilityObserver {
        // SAFETY: the signature matches NSObject's KVO callback. Only our `visible` registration
        // calls it. AppKit mutates the observed window on its main thread.
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observed(
            &self,
            key: Option<&NSString>,
            object: Option<&AnyObject>,
            _change: Option<&NSDictionary>,
            _context: *mut c_void,
        ) {
            if key.is_some_and(|key| key.isEqualToString(ns_string!("visible")))
                && object.is_some_and(|object| {
                    std::ptr::eq(object, &*self.ivars().window as &AnyObject)
                })
            {
                self.publish();
            }
        }
    }
);

impl VisibilityObserver {
    fn new(window: Retained<NSWindow>, handler: Handler, mtm: MainThreadMarker) -> Retained<Self> {
        let observer = Self::alloc(mtm).set_ivars(ObserverState {
            window,
            delivery: Delivery::new(handler),
        });
        // SAFETY: NSObject's initializer is appropriate for our allocated subclass.
        unsafe { msg_send![super(observer), init] }
    }

    fn publish(&self) {
        let state = snapshot(&self.ivars().window, self.mtm());
        self.ivars().delivery.publish(state);
    }
}

struct Monitor {
    center: Retained<NSNotificationCenter>,
    observer: Retained<VisibilityObserver>,
    tokens: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.observer.ivars().delivery.close();
        // SAFETY: each token belongs to this center; KVO removal pairs with the successful
        // registration below. The monitor retains both window and observer until removal ends.
        unsafe {
            for token in &self.tokens {
                self.center.removeObserver((**token).as_ref());
            }
            self.observer
                .ivars()
                .window
                .removeObserver_forKeyPath(&self.observer, ns_string!("visible"));
        }
    }
}

fn window(window: &dyn HasWindowHandle) -> Result<Retained<NSWindow>, String> {
    let handle = window.window_handle().map_err(|error| error.to_string())?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err("Window visibility needs an AppKit window".into());
    };
    // SAFETY: HasWindowHandle lends a valid view during this call, on the main thread. Retain
    // its window before that borrow ends; no borrowed native pointer leaves this boundary.
    let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
    view.window().ok_or("Visibility view has no window".into())
}

fn snapshot(window: &NSWindow, mtm: MainThreadMarker) -> Visibility {
    Visibility {
        supported: true,
        minimized: window.isMiniaturized(),
        // isVisible remains true when covered or on another Space. Never inspect occlusionState.
        window_hidden: !window.isVisible(),
        app_hidden: NSApplication::sharedApplication(mtm).isHidden(),
    }
}

/// Register from the window runtime on AppKit's main thread. Sends the initial facts before
/// returning, then only changed facts. Callbacks must only copy facts and post a bounded wake.
/// Replacement removes all earlier observers. A close notification removes this registration.
pub fn install_visibility_handler(
    handle: &dyn HasWindowHandle,
    handler: Handler,
) -> Result<Visibility, String> {
    let mtm = MainThreadMarker::new().ok_or("Window visibility must start on the main thread")?;
    let window = window(handle)?;
    let center = NSNotificationCenter::defaultCenter();
    let observer = VisibilityObserver::new(window.clone(), handler, mtm);
    let app = NSApplication::sharedApplication(mtm);
    // Removal and registration occur synchronously on the same thread as native window changes.
    MONITOR.with(|slot| slot.borrow_mut().take());
    // SAFETY: KVO invokes the correctly declared method on our retained observer. No raw context
    // is used. Retention pairs with explicit main-thread removal in Monitor::drop.
    unsafe {
        window.addObserver_forKeyPath_options_context(
            &observer,
            ns_string!("visible"),
            NSKeyValueObservingOptions::New,
            std::ptr::null_mut(),
        );
    }
    let mut tokens = Vec::with_capacity(5);
    // SAFETY: these are AppKit's constant notification names. Filters retain the exact window
    // or app. queue=None means synchronous delivery on the native posting thread (main thread).
    unsafe {
        for (name, object, close) in [
            (
                NSWindowDidMiniaturizeNotification,
                &*window as &AnyObject,
                false,
            ),
            (
                NSWindowDidDeminiaturizeNotification,
                &*window as &AnyObject,
                false,
            ),
            (NSApplicationDidHideNotification, &*app as &AnyObject, false),
            (
                NSApplicationDidUnhideNotification,
                &*app as &AnyObject,
                false,
            ),
            (NSWindowWillCloseNotification, &*window as &AnyObject, true),
        ] {
            let observer = observer.clone();
            let block = RcBlock::new(move |_notification: NonNull<NSNotification>| {
                if close {
                    observer.ivars().delivery.close();
                    MONITOR.with(|slot| {
                        let mut slot = slot.borrow_mut();
                        // Ignore a close callback from an older registration if a handler ever
                        // caused nested AppKit work while the replacement was installed.
                        if slot
                            .as_ref()
                            .is_some_and(|monitor| std::ptr::eq(&*monitor.observer, &*observer))
                        {
                            slot.take();
                        }
                    });
                } else {
                    observer.publish();
                }
            });
            tokens.push(center.addObserverForName_object_queue_usingBlock(
                Some(name),
                Some(object),
                None,
                &block,
            ));
        }
    }
    MONITOR.with(|slot| {
        *slot.borrow_mut() = Some(Monitor {
            center,
            observer: observer.clone(),
            tokens,
        });
    });
    // Native changes cannot interleave registration and this snapshot: both are main-thread
    // work, and registration has no Initial callback or queued observer delivery.
    observer.publish();
    Ok(snapshot(&window, mtm))
}

/// Remove the current window's monitor. Safe to call repeatedly from the window runtime.
pub fn uninstall_visibility_handler() -> Result<(), String> {
    MainThreadMarker::new().ok_or("Window visibility must stop on the main thread")?;
    MONITOR.with(|slot| slot.borrow_mut().take());
    Ok(())
}

/// Exercise actual native operations on an isolated background-only evidence app. The window
/// becomes transparent before ordering it in, and no operation makes it key or activates it.
pub fn set_evidence_visibility(
    handle: &dyn HasWindowHandle,
    operation: EvidenceVisibility,
) -> Result<Visibility, String> {
    let mtm = MainThreadMarker::new().ok_or("Native visibility evidence needs the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    if app.activationPolicy() != NSApplicationActivationPolicy::Prohibited {
        return Err("Native visibility evidence requires a background-only app".into());
    }
    let window = window(handle)?;
    window.setAlphaValue(0.0);
    // An ordered-in transparent evidence window must not intercept the person's pointer.
    window.setIgnoresMouseEvents(true);
    match operation {
        EvidenceVisibility::Minimize => window.miniaturize(None),
        EvidenceVisibility::Restore => window.deminiaturize(None),
        EvidenceVisibility::HideWindow => window.orderOut(None),
        EvidenceVisibility::ShowWindow => window.orderFront(None),
        EvidenceVisibility::HideApp => app.hide(None),
        EvidenceVisibility::ShowApp => app.unhideWithoutActivation(),
    }
    Ok(snapshot(&window, mtm))
}
