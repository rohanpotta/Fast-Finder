import SwiftUI

struct SettingsView: View {
    @State private var apiKey: String = ""
    @State private var showKey = false
    @State private var savedMessage: String?
    /// Read from the Rust core, which is the list the indexer actually walks.
    /// This used to be a UserDefaults array that nothing read — the tab showed
    /// three folders and an Add button that changed nothing.
    @State private var indexedFolders: [String] = getIndexedFolders()
    @State private var folderStatus: String?
    @State private var isReindexing = false

    var body: some View {
        TabView {
            aiSettingsTab
                .tabItem {
                    Label("AI Assistant", systemImage: "sparkles")
                }

            foldersTab
                .tabItem {
                    Label("Indexed Folders", systemImage: "folder")
                }
        }
        .frame(width: 480, height: 320)
        .background(WarpTheme.background)
        .onAppear {
            if let stored = KeychainHelper.load() {
                apiKey = stored
            }
        }
    }

    // MARK: - AI Settings

    private var aiSettingsTab: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Claude API Key")
                .font(WarpTheme.titleFont)
                .foregroundColor(WarpTheme.textPrimary)

            Text("Used for AI file operations (Cmd+K). Stored securely in your Keychain.")
                .font(WarpTheme.captionFont)
                .foregroundColor(WarpTheme.textSecondary)

            HStack {
                if showKey {
                    TextField("sk-ant-...", text: $apiKey)
                        .textFieldStyle(.roundedBorder)
                        .font(WarpTheme.monoFont)
                } else {
                    SecureField("sk-ant-...", text: $apiKey)
                        .textFieldStyle(.roundedBorder)
                        .font(WarpTheme.monoFont)
                }
                Button(action: { showKey.toggle() }) {
                    Image(systemName: showKey ? "eye.slash" : "eye")
                }
                .buttonStyle(.plain)
            }

            HStack {
                Button("Save") {
                    if KeychainHelper.save(apiKey: apiKey) {
                        savedMessage = "Saved to Keychain"
                    } else {
                        savedMessage = "Failed to save"
                    }
                    DispatchQueue.main.asyncAfter(deadline: .now() + 2) {
                        savedMessage = nil
                    }
                }
                .buttonStyle(.borderedProminent)
                .tint(Color(WarpTheme.accent))

                if let msg = savedMessage {
                    Text(msg)
                        .font(WarpTheme.captionFont)
                        .foregroundColor(WarpTheme.textSecondary)
                }
            }

            Spacer()
        }
        .padding(24)
    }

    // MARK: - Folders

    private var foldersTab: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Indexed Folders")
                .font(WarpTheme.titleFont)
                .foregroundColor(WarpTheme.textPrimary)

            Text("Searched by the search bar and Recents. Hidden files, ~/Library, and dependency folders like node_modules are always skipped.")
                .font(WarpTheme.captionFont)
                .foregroundColor(WarpTheme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)

            List {
                ForEach(indexedFolders, id: \.self) { folder in
                    HStack {
                        Image(systemName: "folder.fill")
                            .foregroundColor(WarpTheme.accent)
                        Text((folder as NSString).lastPathComponent)
                            .font(WarpTheme.bodyFont)
                        Spacer()
                        Text(abbreviate(folder))
                            .font(WarpTheme.captionFont)
                            .foregroundColor(WarpTheme.textTertiary)
                            .lineLimit(1)
                            .truncationMode(.head)
                    }
                }
                .onDelete { offsets in
                    var next = indexedFolders
                    next.remove(atOffsets: offsets)
                    apply(next)
                }
            }
            .frame(minHeight: 110)

            HStack(spacing: 10) {
                Button("Add Folder...") {
                    let panel = NSOpenPanel()
                    panel.canChooseDirectories = true
                    panel.canChooseFiles = false
                    panel.allowsMultipleSelection = true
                    if panel.runModal() == .OK {
                        apply(indexedFolders + panel.urls.map(\.path))
                    }
                }
                .disabled(isReindexing)

                Button("Reindex Now") { reindex() }
                    .disabled(isReindexing)

                if isReindexing {
                    ProgressView().scaleEffect(0.5)
                }

                Spacer()

                if let msg = folderStatus {
                    Text(msg)
                        .font(WarpTheme.captionFont)
                        .foregroundColor(WarpTheme.textSecondary)
                        .lineLimit(2)
                }
            }
        }
        .padding(24)
    }

    private func abbreviate(_ path: String) -> String {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        return path.hasPrefix(home) ? "~" + path.dropFirst(home.count) : path
    }

    /// Persist the list through the Rust core, then rebuild — widening the
    /// roots means there's content we've never walked, and narrowing them
    /// leaves rows behind that the core prunes for us.
    private func apply(_ folders: [String]) {
        let update = setIndexedFolders(folders: folders)
        indexedFolders = getIndexedFolders()

        var parts: [String] = []
        if !update.rejected.isEmpty {
            let names = update.rejected.map { ($0 as NSString).lastPathComponent }
            parts.append("Skipped \(names.joined(separator: ", "))")
        }
        if update.pruned > 0 {
            parts.append("removed \(update.pruned) stale entries")
        }
        folderStatus = parts.isEmpty ? nil : parts.joined(separator: " · ")

        // Let the running window restart its watcher on the new roots.
        NotificationCenter.default.post(name: .indexedFoldersChanged, object: nil)
        reindex()
    }

    private func reindex() {
        isReindexing = true
        let existing = folderStatus
        Task {
            let count = await Task.detached(priority: .background) {
                rebuildIndex().count
            }.value
            await MainActor.run {
                isReindexing = false
                let indexed = "Indexed \(count) items"
                folderStatus = existing.map { "\($0) · \(indexed)" } ?? indexed
                NotificationCenter.default.post(name: .indexedFoldersChanged, object: nil)
            }
        }
    }
}
