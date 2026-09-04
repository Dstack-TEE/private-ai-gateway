import ServiceManagement
import SwiftUI

@main
struct PrivateAIGatewayApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var store = RuntimeStore.shared

    var body: some Scene {
        WindowGroup("Private AI Gateway", id: "main") {
            MainWindowView(store: store)
                .frame(minWidth: 900, idealWidth: 1052, minHeight: 640, idealHeight: 820)
        }
        .windowResizability(.contentMinSize)
        .commands {
            CommandGroup(replacing: .appSettings) {
                SettingsLink { Text("Settings…") }
                    .keyboardShortcut(",", modifiers: .command)
            }
            CommandMenu("Protection") {
                Button(store.isRunning ? "Stop Protection" : "Start Protection") {
                    store.setProtection(!store.isRunning)
                }
                .disabled(store.isBusy || (!store.state.apiKeySaved && !store.isRunning))
            }
        }

        MenuBarExtra {
            TrayMenu(store: store)
        } label: {
            Image(nsImage: TrayIcon.image(protected: store.isProtected))
        }
        .menuBarExtraStyle(.menu)

        Settings {
            SettingsPage(store: store)
                .frame(width: 620, height: 520)
        }
    }
}

private struct TrayMenu: View {
    @ObservedObject var store: RuntimeStore
    @Environment(\.openWindow) private var openWindow
    @Environment(\.openSettings) private var openSettings
    @State private var openAtLogin = loginItem.status == .enabled

    private var loginItem: SMAppService {
        SMAppService.loginItem(identifier: "org.dstack.private-ai-gateway.login-item")
    }

    var body: some View {
        Toggle("Protected", isOn: Binding(
            get: { store.isRunning },
            set: { store.setProtection($0) }
        ))
        .disabled(store.isBusy || (!store.state.apiKeySaved && !store.isRunning))
        Text(store.isDevMode && store.isRunning ? "Dev mode" : store.state.status.label)
        Divider()
        Button("Open Private AI Gateway") {
            NSApp.activate(ignoringOtherApps: true)
            openWindow(id: "main")
        }
        Button("Settings…") {
            NSApp.activate(ignoringOtherApps: true)
            openSettings()
        }
        Divider()
        Toggle("Open at Login", isOn: Binding(
            get: { openAtLogin },
            set: setOpenAtLogin
        ))
        Divider()
        Button("Quit Private AI Gateway") { NSApp.terminate(nil) }
    }

    private func setOpenAtLogin(_ enabled: Bool) {
        do {
            if enabled { try loginItem.register() }
            else { try loginItem.unregister() }
            openAtLogin = enabled
        } catch {
            store.errorMessage = error.localizedDescription
            openAtLogin = loginItem.status == .enabled
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        let current = ProcessInfo.processInfo.processIdentifier
        if let existing = NSRunningApplication
            .runningApplications(withBundleIdentifier: Bundle.main.bundleIdentifier ?? "")
            .first(where: { $0.processIdentifier != current }) {
            existing.activate(options: [.activateAllWindows, .activateIgnoringOtherApps])
            NSApp.terminate(nil)
            return
        }
        if CommandLine.arguments.contains("--autostart") {
            DispatchQueue.main.async {
                NSApp.windows.forEach { $0.orderOut(nil) }
            }
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }

    func applicationShouldHandleReopen(
        _ sender: NSApplication,
        hasVisibleWindows flag: Bool
    ) -> Bool {
        if !flag {
            sender.windows.first { $0.canBecomeKey }?.makeKeyAndOrderFront(nil)
        }
        return true
    }

    func applicationWillTerminate(_ notification: Notification) {
        RuntimeStore.shared.shutdown()
    }
}

private enum TrayIcon {
    static func image(protected: Bool) -> NSImage {
        let name = protected ? "trayTemplateProtected" : "trayTemplate"
        if let url = Bundle.main.url(forResource: name, withExtension: "png"),
           let image = NSImage(contentsOf: url) {
            image.isTemplate = true
            return image
        }
        let fallback = NSImage(systemSymbolName: protected ? "checkmark.shield.fill" : "shield", accessibilityDescription: "Private AI Gateway") ?? NSImage()
        fallback.isTemplate = true
        return fallback
    }
}

extension GatewayStatus {
    var label: String {
        switch self {
        case .stopped: "Not protected"
        case .verifying: "Verifying…"
        case .verified: "Protected"
        case .blocked: "Blocked"
        case .error: "Needs attention"
        }
    }
}
