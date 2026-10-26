import AppKit

// QuarkBar: Quark in the macOS menubar. A client of quarkd only; see Package.swift.
MainActor.assumeIsolated {
    let app = NSApplication.shared
    let delegate = AppDelegate()
    app.delegate = delegate
    app.setActivationPolicy(.accessory)
    app.run()
}
