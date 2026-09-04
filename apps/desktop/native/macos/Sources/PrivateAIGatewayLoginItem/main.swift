import AppKit

let helperURL = Bundle.main.bundleURL
let appURL = helperURL
    .deletingLastPathComponent()
    .deletingLastPathComponent()
    .deletingLastPathComponent()
    .deletingLastPathComponent()

let configuration = NSWorkspace.OpenConfiguration()
configuration.arguments = ["--autostart"]
configuration.activates = false
NSWorkspace.shared.openApplication(at: appURL, configuration: configuration) { _, _ in
    NSApp.terminate(nil)
}
NSApplication.shared.run()
