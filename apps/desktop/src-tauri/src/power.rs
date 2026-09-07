use desktop_runtime::controller::DesktopRuntime;
use std::sync::{Arc, Weak};
use tauri::AppHandle;

pub fn setup(app: &AppHandle, runtime: &Arc<DesktopRuntime>) {
    if let Err(error) = platform::setup(app, Arc::downgrade(runtime)) {
        runtime.set_wake_monitor_available(false);
        eprintln!("System wake monitoring is unavailable: {error}");
    } else {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        runtime.set_wake_monitor_available(true);
    }
}

pub fn shutdown() {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    platform::shutdown();
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use futures_util::StreamExt;
    use tauri::Manager;
    struct Monitor(tokio::task::AbortHandle);
    impl Drop for Monitor {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    pub fn setup(app: &AppHandle, runtime: Weak<DesktopRuntime>) -> Result<(), String> {
        let task = tauri::async_runtime::spawn(supervise(move || {
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
                    if let Some(runtime) = runtime.upgrade() {
                        runtime.set_wake_monitor_available(true);
                    }
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
                                let Some(runtime) = runtime.upgrade() else { break; };
                                runtime.set_wake_monitor_available(owner.is_some());
                            },
                        }
                    }
                    Ok(())
                }
                .await;
                if let Some(runtime) = runtime.upgrade() {
                    if runtime.set_wake_monitor_available(false) {
                        match result {
                            Err(error) => {
                                eprintln!("System wake monitoring will reconnect: {error}")
                            }
                            Ok(()) => {
                                eprintln!("System wake monitoring stream ended; reconnecting")
                            }
                        }
                    }
                }
            }
        }));
        app.manage(Monitor(task.inner().abort_handle()));
        Ok(())
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
        use std::sync::atomic::{AtomicUsize, Ordering};
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
    use block2::RcBlock;
    use objc2::{rc::Retained, runtime::ProtocolObject, MainThreadMarker};
    use objc2_app_kit::{NSWorkspace, NSWorkspaceDidWakeNotification};
    use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol};
    use std::{cell::RefCell, ptr::NonNull};
    struct Observer {
        center: Retained<NSNotificationCenter>,
        token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
    }
    impl Drop for Observer {
        fn drop(&mut self) {
            let token: &ProtocolObject<dyn NSObjectProtocol> = &self.token;
            // Both retained objects live on the AppKit thread for the registration lifetime.
            unsafe {
                self.center.removeObserver(token.as_ref());
            }
        }
    }
    thread_local! { static OBSERVER: RefCell<Option<Observer>> = const { RefCell::new(None) }; }
    pub fn setup(_app: &AppHandle, runtime: Weak<DesktopRuntime>) -> Result<(), String> {
        MainThreadMarker::new().ok_or("Wake observers require the main thread")?;
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        let callback = RcBlock::new(move |_notification: NonNull<NSNotification>| {
            if let Some(runtime) = runtime.upgrade() {
                runtime.system_resumed();
            }
        });
        // The callback captures only a Send + Sync weak runtime and does not access AppKit.
        let token = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(NSWorkspaceDidWakeNotification),
                None,
                None,
                &callback,
            )
        };
        OBSERVER.with(|observer| *observer.borrow_mut() = Some(Observer { center, token }));
        Ok(())
    }
    pub fn shutdown() {
        OBSERVER.with(|observer| {
            observer.borrow_mut().take();
        });
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use std::{cell::RefCell, ffi::c_void};
    use windows::Win32::{
        Foundation::HANDLE,
        System::Power::{
            PowerRegisterSuspendResumeNotification, PowerUnregisterSuspendResumeNotification,
            DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, HPOWERNOTIFY,
        },
        UI::WindowsAndMessaging::{DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC},
    };
    struct Registration {
        handle: HPOWERNOTIFY,
        runtime: Option<Box<Weak<DesktopRuntime>>>,
    }
    impl Drop for Registration {
        fn drop(&mut self) {
            // Unregister before releasing the callback context.
            let status = unsafe { PowerUnregisterSuspendResumeNotification(self.handle) };
            if status.0 != 0 {
                // A failed unregister cannot prove that callbacks have stopped.
                // Retain this weak context until process exit rather than risk use-after-free.
                if let Some(runtime) = self.runtime.take() {
                    let _ = Box::leak(runtime);
                }
                eprintln!("Could not unregister power notifications ({})", status.0);
            }
        }
    }
    thread_local! { static REGISTRATION: RefCell<Option<Registration>> = const { RefCell::new(None) }; }
    unsafe extern "system" fn resumed(
        context: *const c_void,
        kind: u32,
        _setting: *const c_void,
    ) -> u32 {
        if kind == PBT_APMRESUMEAUTOMATIC {
            // Context is owned by Registration until unregister completes.
            if let Some(runtime) =
                unsafe { context.cast::<Weak<DesktopRuntime>>().as_ref() }.and_then(Weak::upgrade)
            {
                runtime.system_resumed();
            }
        }
        0
    }
    pub fn setup(_app: &AppHandle, runtime: Weak<DesktopRuntime>) -> Result<(), String> {
        let mut runtime = Box::new(runtime);
        let params = DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
            Callback: Some(resumed),
            Context: std::ptr::from_mut(runtime.as_mut()).cast(),
        };
        let mut handle = std::ptr::null_mut();
        // Windows copies the subscription parameters; the heap context stays stable.
        let status = unsafe {
            PowerRegisterSuspendResumeNotification(
                DEVICE_NOTIFY_CALLBACK,
                HANDLE(std::ptr::from_ref(&params).cast_mut().cast()),
                &mut handle,
            )
        };
        if status.0 != 0 {
            return Err(format!(
                "Power notification registration failed ({})",
                status.0
            ));
        }
        REGISTRATION.with(|registration| {
            *registration.borrow_mut() = Some(Registration {
                handle: HPOWERNOTIFY(handle as isize),
                runtime: Some(runtime),
            })
        });
        Ok(())
    }
    pub fn shutdown() {
        REGISTRATION.with(|registration| {
            registration.borrow_mut().take();
        });
    }
}
