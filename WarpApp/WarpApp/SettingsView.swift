import SwiftUI

struct SettingsView: View {
    @State private var apiKey: String = ""
    @State private var showKey = false
    @State private var savedMessage: String?
    @State private var indexedFolders: [String] = {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        return UserDefaults.standard.stringArray(forKey: "indexed_folders") ?? [
            "\(home)/Documents",
            "\(home)/Downloads",
            "\(home)/Desktop"
        ]
    }()

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

            Text("These folders are scanned for the file index and Recents view.")
                .font(WarpTheme.captionFont)
                .foregroundColor(WarpTheme.textSecondary)

            List {
                ForEach(indexedFolders, id: \.self) { folder in
                    HStack {
                        Image(systemName: "folder.fill")
                            .foregroundColor(WarpTheme.accent)
                        Text((folder as NSString).lastPathComponent)
                            .font(WarpTheme.bodyFont)
                        Spacer()
                        Text(folder)
                            .font(WarpTheme.captionFont)
                            .foregroundColor(WarpTheme.textTertiary)
                            .lineLimit(1)
                    }
                }
                .onDelete { offsets in
                    indexedFolders.remove(atOffsets: offsets)
                    saveFolders()
                }
            }
            .frame(minHeight: 120)

            HStack {
                Button("Add Folder...") {
                    let panel = NSOpenPanel()
                    panel.canChooseDirectories = true
                    panel.canChooseFiles = false
                    panel.allowsMultipleSelection = false
                    if panel.runModal() == .OK, let url = panel.url {
                        indexedFolders.append(url.path)
                        saveFolders()
                    }
                }
                Spacer()
            }
        }
        .padding(24)
    }

    private func saveFolders() {
        UserDefaults.standard.set(indexedFolders, forKey: "indexed_folders")
    }
}
