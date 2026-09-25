use std::sync::Weak;

use tokio::runtime::Handle;

use crate::controller::DesktopRuntime;

/// Keeps native system wake notifications registered for its lifetime.
pub struct Monitor {
    platform: Option<platform::Monitor>,
    runtime: Weak<DesktopRuntime>,
}

impl Monitor {
    pub fn start(runtime: Weak<DesktopRuntime>, task_runtime: Handle) -> Result<Self, String> {
        let platform = match platform::Monitor::start(runtime.clone(), task_runtime) {
            Ok(platform) => platform,
            Err(error) => {
                set_available(&runtime, false);
                return Err(error);
            }
        };
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        set_available(&runtime, true);
        Ok(Self {
            platform: Some(platform),
            runtime,
        })
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.platform.take();
        set_available(&self.runtime, false);
    }
}

fn set_available(runtime: &Weak<DesktopRuntime>, available: bool) {
    if let Some(runtime) = runtime.upgrade() {
        runtime.set_wake_monitor_available(available);
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use futures_util::StreamExt;
    use tokio::task::JoinHandle;

    pub struct Monitor(JoinHandle<()>);

    struct AvailabilityGuard(Weak<DesktopRuntime>);

    impl Drop for AvailabilityGuard {
        fn drop(&mut self) {
            set_available(&self.0, false);
        }
    }

    impl Monitor {
        pub fn start(runtime: Weak<DesktopRuntime>, task_runtime: Handle) -> Result<Self, String> {
            let task = task_runtime.spawn(async move {
                let _availability = AvailabilityGuard(runtime.clone());
                supervise(move || {
                    let runtime = runtime.clone();
                    async move {
                        let result: Result<(), zbus::Error> = async {
                            let connection = zbus::Connection::system().await?;
                            let proxy = zbus::Proxy::new(
                                &connection,
                                "org.freedesktop.login1",
                                "/org/freedesktop/login1",
                                "org.freedesktop.login1.Manager",
                            )
                            .await?;
                            let mut owners = proxy.receive_owner_changed().await?;
                            let bus = zbus::fdo::DBusProxy::new(&connection).await?;
                            if !bus.name_has_owner(proxy.destination().clone()).await? {
                                return Err(zbus::Error::Failure("login1 is unavailable".into()));
                            }
                            let mut signals = proxy.receive_signal("PrepareForSleep").await?;
                            set_available(&runtime, true);
                            loop {
                                tokio::select! {
                                    signal = signals.next() => {
                                        let Some(signal) = signal else { break; };
                                        let (sleeping,): (bool,) = signal.body().deserialize()?;
                                        let Some(runtime) = runtime.upgrade() else { break; };
                                        if !sleeping { runtime.system_resumed(); }
                                    },
                                    owner = owners.next() => {
                                        let Some(owner) = owner else { break; };
                                        set_available(&runtime, owner.is_some());
                                    },
                                }
                            }
                            Ok(())
                        }
                        .await;
                        if let Some(runtime) = runtime.upgrade() {
                            if runtime.set_wake_monitor_available(false) {
                                match result {
                                    Err(error) => tracing::warn!(
                                        "System wake monitoring will reconnect: {error}"
                                    ),
                                    Ok(()) => tracing::warn!(
                                        "System wake monitoring stream ended; reconnecting"
                                    ),
                                }
                            }
                        }
                    }
                })
                .await;
            });
            Ok(Self(task))
        }
    }

    impl Drop for Monitor {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    async fn supervise<F, Fut>(mut subscribe: F)
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        loop {
            subscribe().await;
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        #[tokio::test(start_paused = true)]
        async fn ended_subscriptions_retry_without_spinning_and_can_be_cancelled() {
            let attempts = Arc::new(AtomicUsize::new(0));
            let observed = attempts.clone();
            let task = tokio::spawn(supervise(move || {
                let attempts = observed.clone();
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                }
            }));

            tokio::task::yield_now().await;
            assert_eq!(attempts.load(Ordering::SeqCst), 1);
            tokio::time::advance(std::time::Duration::from_secs(4)).await;
            tokio::task::yield_now().await;
            assert_eq!(attempts.load(Ordering::SeqCst), 1);
            tokio::time::advance(std::time::Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
            assert_eq!(attempts.load(Ordering::SeqCst), 2);

            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            tokio::time::advance(std::time::Duration::from_secs(10)).await;
            assert_eq!(attempts.load(Ordering::SeqCst), 2);
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::{
        ffi::c_void,
        ptr,
        sync::{
            atomic::{AtomicU32, Ordering},
            mpsc,
        },
        thread::{self, JoinHandle},
    };

    use objc2_core_foundation::{
        kCFRunLoopCommonModes, CFRetained, CFRunLoop, CFRunLoopSource, CFRunLoopSourceContext,
    };
    use objc2_io_kit::{
        io_connect_t, io_object_t, io_service_t, kIOMessageCanSystemSleep,
        kIOMessageSystemHasPoweredOn, kIOMessageSystemWillSleep, IOAllowPowerChange,
        IODeregisterForSystemPower, IONotificationPort, IONotificationPortRef,
        IORegisterForSystemPower, IOServiceClose,
    };

    struct CallbackContext {
        runtime: Weak<DesktopRuntime>,
        connection: AtomicU32,
    }

    unsafe extern "C-unwind" fn power_changed(
        context: *mut c_void,
        _service: io_service_t,
        message_type: u32,
        message_argument: *mut c_void,
    ) {
        let Some(context) = (unsafe { context.cast::<CallbackContext>().as_ref() }) else {
            return;
        };
        match message_type {
            kIOMessageSystemHasPoweredOn => {
                if let Some(runtime) = context.runtime.upgrade() {
                    runtime.system_resumed();
                }
            }
            kIOMessageCanSystemSleep | kIOMessageSystemWillSleep => {
                let connection = context.connection.load(Ordering::Acquire);
                if connection != 0 {
                    IOAllowPowerChange(connection, message_argument as isize);
                }
            }
            _ => {}
        }
    }

    unsafe extern "C-unwind" fn stop_run_loop(_info: *mut c_void) {
        if let Some(run_loop) = CFRunLoop::current() {
            run_loop.stop();
        }
    }

    /// The worker's run loop and the source that stops it.
    struct Stopper {
        run_loop: CFRetained<CFRunLoop>,
        source: CFRetained<CFRunLoopSource>,
    }

    // SAFETY: another thread only signals the source and wakes the run loop,
    // which Core Foundation allows from any thread; the worker owns the rest.
    unsafe impl Send for Stopper {}

    pub struct Monitor {
        stopper: Stopper,
        thread: Option<JoinHandle<()>>,
    }

    impl Monitor {
        pub fn start(runtime: Weak<DesktopRuntime>, _task_runtime: Handle) -> Result<Self, String> {
            let (started_tx, started_rx) = mpsc::sync_channel(1);
            let thread = thread::Builder::new()
                .name("pap-power-monitor".to_string())
                .spawn(move || run(runtime, started_tx))
                .map_err(|error| format!("Cannot start system wake monitor: {error}"))?;

            match started_rx.recv() {
                Ok(Ok(stopper)) => Ok(Self {
                    stopper,
                    thread: Some(thread),
                }),
                Ok(Err(error)) => {
                    let _ = thread.join();
                    Err(error)
                }
                Err(_) => {
                    let _ = thread.join();
                    Err("System wake monitor stopped during startup".to_string())
                }
            }
        }
    }

    impl Drop for Monitor {
        fn drop(&mut self) {
            // A signaled source stays pending, so cancellation cannot be lost before CFRunLoopRun.
            self.stopper.source.signal();
            self.stopper.run_loop.wake_up();
            if let Some(thread) = self.thread.take() {
                if thread.join().is_err() {
                    tracing::warn!("System wake monitor thread did not shut down cleanly");
                }
            }
        }
    }

    fn run(runtime: Weak<DesktopRuntime>, started: mpsc::SyncSender<Result<Stopper, String>>) {
        let context = Box::into_raw(Box::new(CallbackContext {
            runtime,
            connection: AtomicU32::new(0),
        }));
        let mut port: IONotificationPortRef = ptr::null_mut();
        let mut notifier: io_object_t = 0;
        let connection = unsafe {
            IORegisterForSystemPower(
                context.cast(),
                &mut port,
                Some(power_changed),
                &mut notifier,
            )
        };
        if connection == 0 || port.is_null() || notifier == 0 {
            unsafe {
                if notifier != 0 {
                    IODeregisterForSystemPower(&mut notifier);
                }
                if connection != 0 {
                    IOServiceClose(connection);
                }
                if !port.is_null() {
                    IONotificationPort::destroy(port);
                }
                drop(Box::from_raw(context));
            }
            let _ = started.send(Err(
                "IOKit could not register system wake notifications".to_string()
            ));
            return;
        }
        unsafe { (*context).connection.store(connection, Ordering::Release) };

        let source = unsafe { IONotificationPort::run_loop_source(port) };
        let mut stop_context = CFRunLoopSourceContext {
            version: 0,
            info: ptr::null_mut(),
            retain: None,
            release: None,
            copyDescription: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform: Some(stop_run_loop),
        };
        let stop_source = unsafe { CFRunLoopSource::new(None, 0, &mut stop_context) };
        let (Some(source), Some(stop_source), Some(run_loop)) =
            (source, stop_source, CFRunLoop::current())
        else {
            cleanup(connection, port, &mut notifier, context);
            let _ = started.send(Err(
                "Core Foundation could not create the wake monitor run loop".to_string(),
            ));
            return;
        };

        let modes = unsafe { kCFRunLoopCommonModes };
        run_loop.add_source(Some(&source), modes);
        run_loop.add_source(Some(&stop_source), modes);
        let stopper = Stopper {
            run_loop: run_loop.clone(),
            source: stop_source.clone(),
        };
        if started.send(Ok(stopper)).is_ok() {
            CFRunLoop::run();
        }
        run_loop.remove_source(Some(&stop_source), modes);
        run_loop.remove_source(Some(&source), modes);
        cleanup(connection, port, &mut notifier, context);
    }

    fn cleanup(
        connection: io_connect_t,
        port: IONotificationPortRef,
        notifier: &mut io_object_t,
        context: *mut CallbackContext,
    ) {
        let deregistered = unsafe { IODeregisterForSystemPower(notifier) };
        let closed = IOServiceClose(connection);
        unsafe {
            IONotificationPort::destroy(port);
            drop(Box::from_raw(context));
        }
        if deregistered != 0 {
            tracing::warn!("Could not unregister macOS power notifications ({deregistered})");
        }
        if closed != 0 {
            tracing::warn!("Could not close the macOS power connection ({closed})");
        }
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "windows")))]
mod tests {
    use super::*;

    #[test]
    fn native_monitor_can_start_and_stop_repeatedly() {
        let task_runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("Tokio runtime should start");

        for attempt in 1..=8 {
            let monitor =
                Monitor::start(Weak::<DesktopRuntime>::new(), task_runtime.handle().clone())
                    .unwrap_or_else(|error| {
                        panic!(
                            "Native power monitor registration failed on attempt {attempt}; \
                     this test requires the normal CI process to register OS power notifications: \
                     {error}"
                        )
                    });
            drop(monitor);
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use std::{ffi::c_void, ptr, sync::Mutex};
    use windows_sys::Win32::{
        System::Power::{
            PowerRegisterSuspendResumeNotification, PowerUnregisterSuspendResumeNotification,
            DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, HPOWERNOTIFY,
        },
        UI::WindowsAndMessaging::{DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC},
    };

    struct CallbackSlot {
        runtime: Option<Weak<DesktopRuntime>>,
        reusable: bool,
    }

    static CALLBACK: Mutex<CallbackSlot> = Mutex::new(CallbackSlot {
        runtime: None,
        reusable: true,
    });

    unsafe extern "system" fn power_changed(
        _context: *const c_void,
        event: u32,
        _setting: *const c_void,
    ) -> u32 {
        if event == PBT_APMRESUMEAUTOMATIC {
            let runtime = CALLBACK
                .lock()
                .ok()
                .and_then(|slot| slot.runtime.as_ref().and_then(Weak::upgrade));
            if let Some(runtime) = runtime {
                runtime.system_resumed();
            }
        }
        0
    }

    pub struct Monitor(HPOWERNOTIFY);

    impl Monitor {
        pub fn start(runtime: Weak<DesktopRuntime>, _task_runtime: Handle) -> Result<Self, String> {
            let mut slot = CALLBACK
                .lock()
                .map_err(|_| "System wake callback state is unavailable".to_string())?;
            if !slot.reusable {
                return Err("System wake callbacks cannot be registered again".to_string());
            }
            if slot.runtime.is_some() {
                return Err("System wake monitoring is already active".to_string());
            }
            slot.runtime = Some(runtime);
            drop(slot);

            let params = DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
                Callback: Some(power_changed),
                Context: ptr::null_mut(),
            };
            let mut handle = ptr::null_mut();
            let status = unsafe {
                PowerRegisterSuspendResumeNotification(
                    DEVICE_NOTIFY_CALLBACK,
                    ptr::from_ref(&params).cast_mut().cast(),
                    &mut handle,
                )
            };
            if status != 0 {
                if let Ok(mut slot) = CALLBACK.lock() {
                    slot.runtime = None;
                }
                return Err(format!("Power notification registration failed ({status})"));
            }
            Ok(Self(handle as HPOWERNOTIFY))
        }
    }

    impl Drop for Monitor {
        fn drop(&mut self) {
            let status = unsafe { PowerUnregisterSuspendResumeNotification(self.0) };
            match CALLBACK.lock() {
                Ok(mut slot) => {
                    slot.runtime = None;
                    if status != 0 {
                        slot.reusable = false;
                    }
                }
                Err(_) => {
                    tracing::warn!("System wake callback state is unavailable during shutdown")
                }
            }
            if status != 0 {
                tracing::warn!("Could not unregister power notifications ({status})");
            }
        }
    }
}
