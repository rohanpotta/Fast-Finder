//
//  WarpAppApp.swift
//  WarpApp
//
//  Created by Rohan Potta on 1/10/26.
//

import SwiftUI

@main
struct WarpAppApp: App {
    init() {
        // Move any legacy plaintext API key out of UserDefaults into the
        // Keychain. Idempotent — safe to run every launch.
        AIService.migrateAPIKeyOnLaunch()
    }

    var body: some Scene {
        WindowGroup {
            ContentView()
        }
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: 900, height: 600)
        .commands {
            // Standard SwiftUI Edit menu doesn't know about file-op undo.
            // We replace the default undo command with one that calls into
            // the Rust block store so Cmd+Z reverses move/trash/rename/etc.
            CommandGroup(replacing: .undoRedo) {
                Button("Undo File Action") {
                    NotificationCenter.default.post(name: .undoLastBlock, object: nil)
                }
                .keyboardShortcut("z", modifiers: .command)
            }
        }

        Settings {
            SettingsView()
        }
    }
}

extension Notification.Name {
    static let undoLastBlock = Notification.Name("WarpApp.undoLastBlock")
    /// Posted when the indexed-folder list changes, so the open window can
    /// point its file watcher at the new roots and refresh.
    static let indexedFoldersChanged = Notification.Name("WarpApp.indexedFoldersChanged")
}
