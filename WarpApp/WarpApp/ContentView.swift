//
//  ContentView.swift
//  WarpApp
//
//  Created by Rohan Potta on 1/10/26.
//

import SwiftUI
import Quartz // For Quick Look preview
import QuickLookThumbnailing

// Sidebar item model
enum SidebarItem: String, CaseIterable, Identifiable {
    case recents = "Recents"
    case applications = "Applications"
    case desktop = "Desktop"
    case documents = "Documents"
    case downloads = "Downloads"
    case user = "User" // Will be replaced with actual username
    case trash = "Trash"

    var id: String { rawValue }

    // Get the actual display name (user gets the real username)
    var displayName: String {
        if self == .user {
            return NSUserName() // Returns actual username like "ropo"
        }
        return self.rawValue
    }

    var icon: String {
        switch self {
        case .recents: return "clock.fill"
        case .applications: return "square.grid.2x2.fill"
        case .desktop: return "desktopcomputer"
        case .documents: return "doc.fill"
        case .downloads: return "arrow.down.circle.fill"
        case .user: return "person.fill"
        case .trash: return "trash.fill"
        }
    }

    var path: String {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        switch self {
        case .recents: return ""
        case .applications: return "/Applications"
        case .desktop: return "\(home)/Desktop"
        case .documents: return "\(home)/Documents"
        case .downloads: return "\(home)/Downloads"
        case .user: return home
        case .trash: return "\(home)/.Trash"
        }
    }
}

// Make SearchResult (from UniFFI) work with SwiftUI Table
extension SearchResult: Identifiable {
    public var id: String { filePath }
}

struct ContentView: View {
    @State private var query = ""
    @State private var results: [SearchResult] = []
    @State private var searchTask: Task<Void, Never>? = nil
    @State private var selectedFileIds: Set<String> = []  // Multi-select
    @State private var selectedSidebarItem: SidebarItem = .recents
    @State private var sortOrder = [KeyPathComparator(\SearchResult.dateValue, order: .reverse)]

    // File operation state
    @State private var showRenameSheet = false
    @State private var renameText = ""
    @State private var showMovePanel = false

    // AI state
    @State private var showAIBar = false
    @State private var aiPrefilledPlan: AIActionPlan?

    // NL detection
    @State private var isNLQuery: Bool = false

    /// Which date organises the list. Persisted so it survives relaunches —
    /// it's a stance about how you work, not a per-session toggle.
    @AppStorage("date_field") private var dateField: DateFieldChoice = .either

    // Transient toast (undo result, AI errors, etc.)
    @State private var toastMessage: String? = nil
    @State private var toastIsError: Bool = false

    // Drill-in navigation (open folder = show only its contents; Back pops)
    @State private var navigationPathStack: [String] = []

    /// The folder path we're currently viewing (drilled-in path or sidebar selection).
    private var effectiveCurrentPath: String? {
        if let last = navigationPathStack.last { return last }
        return selectedSidebarItem == .recents ? nil : selectedSidebarItem.path
    }

    /// Expand/collapse folders in the main file list (arrow = expand inline).
    @State private var loadedFolderContents: [String: [SearchResult]] = [:]
    /// Path we just loaded so the outline view can reload that item.
    @State private var lastExpandedPath: String? = nil

    // File watcher. Roots come from the indexed-folder setting at start time,
    // not from a hardcoded list, so editing that setting actually takes effect.
    @StateObject private var fileWatcher = FileWatcher()

    // Status bar computed properties
    private var selectedTotalSize: UInt64 {
        let allFiles = results + loadedFolderContents.values.flatMap { $0 }
        return allFiles.filter { selectedFileIds.contains($0.filePath) }
            .reduce(0) { $0 + $1.fileSize }
    }

    var body: some View {
        HStack(spacing: 0) {
            // --- SIDEBAR ---
            SidebarView(
                selectedItem: $selectedSidebarItem,
                onDrop: { providers, item in
                    handleDrop(providers: providers, to: item)
                }
            )

            // Vertical divider
            Rectangle()
                .fill(WarpTheme.divider)
                .frame(width: 1)

            // --- DETAIL AREA ---
            VStack(spacing: 0) {
                // Breadcrumb navigation
                BreadcrumbBar(
                    sidebarItem: selectedSidebarItem,
                    navigationStack: navigationPathStack,
                    onNavigateToIndex: { index in
                        navigateToStackIndex(index)
                    }
                )

                Rectangle().fill(WarpTheme.divider).frame(height: 1)

                // Search bar, with the date-field control alongside it
                HStack(spacing: 8) {
                    SearchBarView(
                        query: $query,
                        isNLDetected: isNLQuery,
                        onSubmit: { handleSearchSubmit() }
                    )
                    DateFieldPicker(choice: $dateField)
                        .padding(.trailing, 12)
                }

                Rectangle().fill(WarpTheme.divider).frame(height: 1)

                // --- OUTLINE VIEW ---
                FileOutlineView(
                    files: sortedResults,
                    selection: $selectedFileIds,
                    loadedFolderContents: loadedFolderContents,
                    lastExpandedPath: $lastExpandedPath,
                    dateColumnTitle: dateField.label,
                    onFolderExpanded: { path in
                        guard loadedFolderContents[path] == nil else { return }
                        let field = dateField
                        Task {
                            let contents = await Task.detached(priority: .userInitiated) {
                                loadDirectoryContents(path: path, showHidden: false, field: field)
                            }.value
                            await MainActor.run {
                                loadedFolderContents[path] = contents.sorted(using: sortOrder)
                                lastExpandedPath = path
                            }
                        }
                    },
                    onDoubleClick: { path in
                        let allShown = sortedResults + loadedFolderContents.values.flatMap { $0 }
                        let isFolder = allShown.first { $0.filePath == path }?.isFolder ?? false
                        if isFolder {
                            withAnimation(.spring(response: 0.3)) {
                                navigateIntoFolder(path)
                            }
                        } else {
                            openFile(path)
                        }
                    }
                )
                .contextMenu {
                    // Quick Look (single file only)
                    if selectedFileIds.count == 1 {
                        Button("Quick Look") {
                            QuickLookController.shared.togglePreview(for: selectedFileIds.first)
                        }
                        .keyboardShortcut(.space, modifiers: [])
                    }

                    Divider()

                    Button("Open") {
                        for path in selectedFileIds {
                            openFile(path)
                        }
                    }
                    .keyboardShortcut(.return, modifiers: [])

                    // Rename (single file only)
                    if selectedFileIds.count == 1 {
                        Button("Rename...") {
                            if let path = selectedFileIds.first {
                                renameText = URL(fileURLWithPath: path).lastPathComponent
                                showRenameSheet = true
                            }
                        }
                    }

                    Divider()

                    Button("Move to Trash") {
                        let paths = Array(selectedFileIds)
                        trashFilesNative(paths: paths)
                    }
                    .keyboardShortcut(.delete, modifiers: .command)

                    Button("Copy Path") {
                        let paths = selectedFileIds.joined(separator: "\n")
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(paths, forType: .string)
                    }
                    .keyboardShortcut("c", modifiers: .command)

                    if !selectedFileIds.isEmpty {
                        Button("Compress \(selectedFileIds.count) item\(selectedFileIds.count > 1 ? "s" : "")...") {
                            compressSelected()
                        }
                    }

                    Divider()

                    Button("Show in Finder") {
                        if let path = selectedFileIds.first {
                            NSWorkspace.shared.selectFile(path, inFileViewerRootedAtPath: "")
                        }
                    }
                }
                .onKeyPress(.return) {
                    for path in selectedFileIds {
                        openFile(path)
                    }
                    return .handled
                }
                .onKeyPress(.space) {
                    QuickLookController.shared.togglePreview(for: selectedFileIds.first)
                    return .handled
                }
                .onKeyPress(.delete) {
                    let paths = Array(selectedFileIds)
                    trashFilesNative(paths: paths)
                    return .handled
                }

                // Status Bar
                StatusBar(
                    fileCount: results.count,
                    selectedCount: selectedFileIds.count,
                    selectedSize: selectedTotalSize
                )
            }
            .background(WarpTheme.surfacePrimary)
        }
        .frame(minWidth: 750, minHeight: 500)
        .background(WarpTheme.background)
        .background(QuickLookHost())
        .task {
            loadFolder(.recents)
            await startWatching()
        }
        .onChange(of: selectedSidebarItem) { newItem in
            withAnimation(.spring(response: 0.3)) {
                navigationPathStack = []
                loadedFolderContents.removeAll()
            }
            loadFolder(newItem)
        }
        .onChange(of: dateField) { _ in
            // Changing the field changes which rows qualify, not just their
            // order, so the current view has to be re-fetched rather than
            // re-sorted locally.
            if query.isEmpty {
                refreshCurrentContent()
            } else {
                runSearch(for: query)
            }
        }
        .onChange(of: query) { newValue in
            let detected = NLDetector.isNaturalLanguage(newValue)
            withAnimation(.easeInOut(duration: 0.2)) {
                isNLQuery = detected
            }
            if !detected {
                runSearch(for: newValue)
            }
        }
        .onReceive(NotificationCenter.default.publisher(for: .undoLastBlock)) { _ in
            handleUndo()
        }
        .onReceive(NotificationCenter.default.publisher(for: .indexedFoldersChanged)) { _ in
            restartWatching()
        }
        .overlay(alignment: .top) {
            if let msg = toastMessage {
                ToastBanner(message: msg, isError: toastIsError)
                    .padding(.top, 12)
                    .transition(.move(edge: .top).combined(with: .opacity))
            }
        }
        .sheet(isPresented: $showRenameSheet) {
            RenameSheet(
                currentName: renameText,
                onRename: { newName in
                    if let path = selectedFileIds.first {
                        let result = renameFile(path: path, newName: newName)
                        if result.success {
                            refreshCurrentContent()
                        }
                    }
                    showRenameSheet = false
                },
                onCancel: { showRenameSheet = false }
            )
        }
        // AI Input Overlay (Cmd+K)
        .overlay(alignment: .top) {
            if showAIBar {
                AIInputBar(
                    isPresented: $showAIBar,
                    currentFolder: .constant(effectiveCurrentPath ?? selectedSidebarItem.path),
                    selectedFiles: .constant(Array(selectedFileIds)),
                    prefilledPlan: $aiPrefilledPlan,
                    onExecute: { plan, userQuery in
                        executeAIPlan(plan, userQuery: userQuery)
                        if plan.action != .search {
                            showAIBar = false
                        }
                    }
                )
                .padding(.top, 60)
                .transition(.move(edge: .top).combined(with: .opacity))
            }
        }
        .animation(.spring(response: 0.3), value: showAIBar)
        // Cmd+K to toggle AI bar
        .background(
            Button("") {
                showAIBar.toggle()
            }
            .keyboardShortcut("k", modifiers: .command)
            .hidden()
        )
    }

    // Sorted results based on current sort order
    var sortedResults: [SearchResult] {
        results.sorted(using: sortOrder)
    }

    // MARK: - Search Submit (NL detection)

    func handleSearchSubmit() {
        if isNLQuery {
            // Open AI bar with the query
            showAIBar = true
            aiPrefilledPlan = nil
            // The AI bar will handle submission
            Task {
                do {
                    let plan = try await AIService.shared.parseCommand(
                        userQuery: query,
                        currentFolder: effectiveCurrentPath ?? selectedSidebarItem.path,
                        selectedFiles: Array(selectedFileIds)
                    )
                    await MainActor.run {
                        aiPrefilledPlan = plan
                    }
                } catch {
                    print("NL query AI error: \(error)")
                }
            }
        } else {
            openSelected()
        }
    }

    // MARK: - Breadcrumb Navigation

    func navigateToStackIndex(_ index: Int) {
        if index < 0 {
            // Navigate to sidebar root
            withAnimation(.spring(response: 0.3)) {
                navigationPathStack = []
                loadedFolderContents.removeAll()
            }
            refreshCurrentContent()
        } else if index < navigationPathStack.count - 1 {
            // Navigate to intermediate path
            let targetPath = navigationPathStack[index]
            withAnimation(.spring(response: 0.3)) {
                navigationPathStack = Array(navigationPathStack.prefix(index + 1))
                loadedFolderContents.removeAll()
            }
            loadFolderContents(path: targetPath)
        }
    }

    // MARK: - Native Trash

    func trashFilesNative(paths: [String]) {
        // Delegated to the Rust core so the action is recorded as a block —
        // that's what makes Cmd+Z work afterwards. We lose Finder's native
        // "Put Back" xattr, but in-app undo is more discoverable.
        let result = trashFiles(paths: paths)
        if result.affectedCount > 0 {
            results.removeAll { paths.contains($0.filePath) }
            selectedFileIds.removeAll()
        }
        if !result.success {
            showToast(result.message, isError: true)
        }
    }

    // MARK: - Undo + toast

    private func handleUndo() {
        Task {
            let result = await Task.detached(priority: .userInitiated) { undoLastBlock() }.value
            await MainActor.run {
                showToast(result.message, isError: !result.success)
                if result.success { refreshCurrentContent() }
            }
        }
    }

    private func showToast(_ message: String, isError: Bool) {
        withAnimation(.easeOut(duration: 0.15)) {
            toastMessage = message
            toastIsError = isError
        }
        Task {
            try? await Task.sleep(nanoseconds: 2_500_000_000)
            await MainActor.run {
                withAnimation(.easeIn(duration: 0.2)) { toastMessage = nil }
            }
        }
    }

    // --- ACTIONS ---

    func executeAIPlan(_ plan: AIActionPlan, userQuery: String) {
        // Handle search: run search, then re-call AI with results so it can return moveFiles/trash/etc.
        if plan.action == .search {
            let searchQuery = plan.searchQuery ?? userQuery
            print("AI searching for: \(searchQuery)")
            self.query = searchQuery
            Task {
                let searchResults = await runSearchAndGetResults(for: searchQuery)
                await MainActor.run {
                    self.results = searchResults
                    self.selectedFileIds = searchResults.first.map { [$0.filePath] } ?? []
                }
                do {
                    let newPlan = try await AIService.shared.parseCommand(
                        userQuery: userQuery,
                        currentFolder: selectedSidebarItem.path,
                        selectedFiles: [],
                        recentSearchResults: searchResults
                    )
                    await MainActor.run {
                        aiPrefilledPlan = newPlan
                    }
                } catch {
                    await MainActor.run {
                        aiPrefilledPlan = AIActionPlan(
                            action: .unknown,
                            sourcePaths: nil,
                            destination: nil,
                            searchQuery: nil,
                            explanation: "Search completed but couldn't interpret next step: \(error.localizedDescription)"
                        )
                    }
                }
            }
            return
        }

        // For file operations, we need source paths
        guard let paths = plan.sourcePaths, !paths.isEmpty else {
            print("AI returned no files to act on: \(plan.explanation)")
            return
        }

        switch plan.action {
        case .moveFiles:
            if let dest = plan.destination {
                let result = moveFiles(sourcePaths: paths, destination: dest)
                if result.success {
                    showToast("Moved \(result.affectedCount) item(s)", isError: false)
                    refreshCurrentContent()
                } else {
                    showToast(result.message, isError: true)
                }
            }
        case .trashFiles:
            trashFilesNative(paths: paths)
        case .compressFiles:
            let firstPath = paths[0] as NSString
            let parentDir = firstPath.deletingLastPathComponent
            let archivePath = "\(parentDir)/AI_Archive_\(Int(Date().timeIntervalSince1970)).zip"

            let result = compressFiles(paths: paths, archivePath: archivePath)
            if result.success {
                showToast("Archived \(result.affectedCount) item(s)", isError: false)
                refreshCurrentContent()
            } else {
                showToast(result.message, isError: true)
            }
        case .search, .unknown:
            print("Unhandled action: \(plan.action)")
        }
    }

    func loadFolder(_ item: SidebarItem) {
        query = ""
        if item == .recents {
            loadRecents()
            return
        }
        loadFolderContents(path: item.path, showHidden: item == .trash)
    }

    /// Load a folder's contents into the main area (used for sidebar selection and drill-in).
    func loadFolderContents(path: String, showHidden: Bool = false) {
        loadedFolderContents.removeAll()
        let field = dateField
        Task {
            let contents = await Task.detached(priority: .userInitiated) {
                return loadDirectoryContents(path: path, showHidden: showHidden, field: field)
            }.value
            await MainActor.run {
                self.results = contents
                self.selectedFileIds = contents.first.map { [$0.filePath] } ?? []
            }
        }
    }

    /// Push a subfolder and show only its contents (Back will pop).
    func navigateIntoFolder(_ path: String) {
        navigationPathStack.append(path)
        loadFolderContents(path: path)
    }

    /// Pop the drill-in stack and show the previous folder (or sidebar root).
    func goBack() {
        guard !navigationPathStack.isEmpty else { return }
        withAnimation(.spring(response: 0.3)) {
            navigationPathStack.removeLast()
        }
        if let path = navigationPathStack.last {
            loadFolderContents(path: path)
        } else {
            refreshCurrentContent()
        }
    }

    /// Reload current view (after move/compress etc.).
    func refreshCurrentContent() {
        query = ""
        if let path = effectiveCurrentPath {
            let isTrash = (path == (FileManager.default.homeDirectoryForCurrentUser.path as NSString).appendingPathComponent(".Trash"))
            loadFolderContents(path: path, showHidden: isTrash)
        } else {
            loadRecents()
        }
    }

    func loadRecents() {
        // One indexed query, already filtered and capped in SQL. The old path
        // pulled the whole index (up to 50k rows) across the FFI boundary and
        // then filtered it in Swift.
        let field = dateField.ffi
        Task {
            let recent = await Task.detached(priority: .userInitiated) {
                return getRecentFiles(dateField: field, withinDays: 7)
            }.value

            await MainActor.run {
                guard self.query.isEmpty, self.selectedSidebarItem == .recents else { return }
                self.results = recent
                self.selectedFileIds = recent.first.map { [$0.filePath] } ?? []
            }
        }
    }

    // MARK: - Index maintenance

    /// Resume the FSEvents stream where the last run left off, then bring the
    /// index up to date. A full rescan only runs on first launch or when the
    /// stream has gone stale — the steady state is `indexPaths` on each batch.
    private func startWatching() async {
        // Read the resume point before any rescan, then run the rescan to
        // completion before the stream goes live. That keeps the full scan and
        // the incremental writer off each other's toes, and costs nothing:
        // anything that changed during the scan is still replayed, because we
        // resume from the pre-scan id and indexing a path twice is a no-op.
        let resumeId = await Task.detached(priority: .utility) { lastEventId() }.value

        if await Task.detached(priority: .utility) { needsFullRescan() }.value {
            _ = await Task.detached(priority: .background) { rebuildIndex() }.value
            await MainActor.run {
                if query.isEmpty { refreshCurrentContent() }
            }
        }

        let roots = await Task.detached(priority: .utility) { getIndexedFolders() }.value
        fileWatcher.onChange = { batch in
            handleFileChanges(batch)
        }
        fileWatcher.start(paths: roots, sinceEventId: resumeId)
    }

    /// Re-point the watcher after the indexed-folder list changes. Without this
    /// the app would keep watching the old roots until relaunch, so edits to a
    /// newly added folder would go unnoticed.
    private func restartWatching() {
        Task {
            let roots = await Task.detached(priority: .utility) { getIndexedFolders() }.value
            await MainActor.run {
                fileWatcher.restart(paths: roots)
                if query.isEmpty { refreshCurrentContent() }
            }
        }
    }

    /// Fold one FSEvents batch into the index and refresh only if it actually
    /// changed something — most batches touch paths we don't index.
    private func handleFileChanges(_ batch: FileChangeBatch) {
        Task {
            let changed = await Task.detached(priority: .utility) { () -> Bool in
                if batch.needsFullRescan {
                    _ = rebuildIndex()
                    return true
                }
                let update = indexPaths(paths: batch.paths)
                return update.upserted > 0 || update.removed > 0
            }.value

            // Only advance the resume point once the batch is durably indexed,
            // so a crash mid-batch replays it instead of losing it.
            if batch.latestEventId > 0 {
                await Task.detached(priority: .utility) {
                    setLastEventId(id: batch.latestEventId)
                }.value
            }

            guard changed else { return }
            await MainActor.run {
                // Don't yank the list out from under an active search.
                if query.isEmpty { refreshCurrentContent() }
            }
        }
    }

    /// Live listing for browsing a folder. Not served from the index — a folder
    /// you're looking at should show what's on disk right now.
    ///
    /// `field` has to be passed in rather than read from `dateField` because
    /// this runs on a detached task; it also has to be honoured here at all,
    /// or the picker would silently only affect Recents and search.
    func loadDirectoryContents(
        path: String,
        showHidden: Bool = false,
        field: DateFieldChoice = .either
    ) -> [SearchResult] {
        var items: [SearchResult] = []
        let fileManager = FileManager.default

        do {
            let contents = try fileManager.contentsOfDirectory(atPath: path)
            for name in contents {
                // Skip hidden files unless showHidden is true
                if !showHidden && name.hasPrefix(".") { continue }

                let fullPath = (path as NSString).appendingPathComponent(name)
                var isDir: ObjCBool = false

                if fileManager.fileExists(atPath: fullPath, isDirectory: &isDir) {
                    let attrs = try? fileManager.attributesOfItem(atPath: fullPath)
                    let size = (attrs?[.size] as? UInt64) ?? 0

                    let modDate = (attrs?[.modificationDate] as? Date) ?? Date.distantPast
                    let createDate = (attrs?[.creationDate] as? Date) ?? Date.distantPast

                    // Mirrors DateField in the Rust core: the chosen field wins
                    // outright, and only `.either` falls back to "newer of the two".
                    let (bestDate, dateKind): (Date, String) = {
                        switch field {
                        case .added:
                            return (createDate, "Added")
                        case .modified:
                            return (modDate, "Modified")
                        case .either:
                            return createDate > modDate
                                ? (createDate, "Added")
                                : (modDate, "Modified")
                        }
                    }()

                    let dateValue = Int64(bestDate.timeIntervalSince1970)
                    let fileKind = getFileKind(path: fullPath, isFolder: isDir.boolValue)

                    items.append(SearchResult(
                        fileName: name,
                        filePath: fullPath,
                        fileSize: size,
                        isFolder: isDir.boolValue,
                        score: dateValue,
                        dateValue: dateValue,
                        dateKind: dateKind,
                        fileKind: fileKind,
                        prettyDate: formatRelativeDate(dateValue)
                    ))
                }
            }
        } catch {
            print("Error loading directory: \(error)")
        }

        // Sort by date (most recent first)
        items.sort { $0.dateValue > $1.dateValue }

        return items
    }

    func getFileKind(path: String, isFolder: Bool) -> String {
        if isFolder { return "Folder" }

        let ext = (path as NSString).pathExtension.lowercased()
        switch ext {
        case "pdf": return "PDF Document"
        case "doc", "docx": return "Word Document"
        case "xls", "xlsx": return "Excel Spreadsheet"
        case "ppt", "pptx": return "Presentation"
        case "txt": return "Plain Text"
        case "md": return "Markdown"
        case "html", "htm": return "HTML Document"
        case "js": return "JavaScript"
        case "ts": return "TypeScript"
        case "py": return "Python Script"
        case "swift": return "Swift Source"
        case "rs": return "Rust Source"
        case "json": return "JSON"
        case "jpg", "jpeg": return "JPEG Image"
        case "png": return "PNG Image"
        case "gif": return "GIF Image"
        case "heic": return "HEIC Image"
        case "mp4": return "MP4 Video"
        case "mov": return "QuickTime Movie"
        case "mp3": return "MP3 Audio"
        case "zip": return "ZIP Archive"
        case "dmg": return "Disk Image"
        case "app": return "Application"
        default: return ext.isEmpty ? "Document" : "\(ext.uppercased()) File"
        }
    }

    func formatRelativeDate(_ timestamp: Int64) -> String {
        let now = Int64(Date().timeIntervalSince1970)
        let diff = now - timestamp

        if diff < 60 { return "Just now" }
        if diff < 3600 { return "\(diff / 60)m ago" }
        if diff < 86400 { return "\(diff / 3600)h ago" }
        if diff < 604800 { return "\(diff / 86400)d ago" }
        if diff < 2592000 { return "\(diff / 604800)w ago" }
        if diff < 31536000 { return "\(diff / 2592000)mo ago" }
        return "\(diff / 31536000)y ago"
    }

    func runSearch(for text: String) {
        searchTask?.cancel()

        if text.isEmpty {
            refreshCurrentContent()
            return
        }

        searchTask = Task {
            let newResults = await runSearchAndGetResults(for: text)
            await MainActor.run {
                self.results = newResults
                self.selectedFileIds = newResults.first.map { [$0.filePath] } ?? []
            }
        }
    }

    /// Runs search and returns results (used by AI flow so we can pass results back to Claude).
    func runSearchAndGetResults(for text: String) async -> [SearchResult] {
        guard !text.isEmpty else { return [] }
        try? await Task.sleep(nanoseconds: 100_000_000)
        if Task.isCancelled { return [] }
        let field = dateField.ffi
        return await Task.detached(priority: .userInitiated) {
            searchFiles(query: text, dateField: field)
        }.value
    }

    func openSelected() {
        let path = selectedFileIds.first ?? results.first?.filePath
        guard let path = path else { return }
        let isFolder = results.first { $0.filePath == path }?.isFolder ?? false
        if isFolder {
            withAnimation(.spring(response: 0.3)) {
                navigateIntoFolder(path)
            }
        } else {
            openFile(path)
        }
    }

    func openFile(_ path: String) {
        let url = URL(fileURLWithPath: path)
        NSWorkspace.shared.open(url)
    }

    func formattedDate(_ timestamp: Int64) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(timestamp))
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .abbreviated
        return formatter.localizedString(for: date, relativeTo: Date())
    }

    func compressSelected() {
        guard let firstPath = selectedFileIds.first else { return }
        let parentDir = URL(fileURLWithPath: firstPath).deletingLastPathComponent().path
        let archiveName = selectedFileIds.count == 1
            ? URL(fileURLWithPath: firstPath).deletingPathExtension().lastPathComponent + ".zip"
            : "Archive.zip"
        let archivePath = (parentDir as NSString).appendingPathComponent(archiveName)

        let paths = Array(selectedFileIds)
        let result = compressFiles(paths: paths, archivePath: archivePath)

        if result.success {
            refreshCurrentContent()
        }
    }

    // Handle dropping files onto sidebar items
    func handleDrop(providers: [NSItemProvider], to item: SidebarItem) -> Bool {
        // Can't drop onto Recents
        guard item != .recents else { return false }

        var droppedPaths: [String] = []
        let group = DispatchGroup()

        for provider in providers {
            if provider.hasItemConformingToTypeIdentifier("public.file-url") {
                group.enter()
                provider.loadItem(forTypeIdentifier: "public.file-url", options: nil) { data, error in
                    defer { group.leave() }
                    if let data = data as? Data,
                       let url = URL(dataRepresentation: data, relativeTo: nil) {
                        droppedPaths.append(url.path)
                    }
                }
            }
        }

        group.notify(queue: .main) {
            guard !droppedPaths.isEmpty else { return }

            if item == .trash {
                trashFilesNative(paths: droppedPaths)
            } else {
                // Move to folder
                let destination = item.path
                let result = moveFiles(sourcePaths: droppedPaths, destination: destination)
                if result.success {
                    self.results.removeAll { droppedPaths.contains($0.filePath) }
                    self.selectedFileIds.removeAll()
                }
            }
        }

        return true
    }
}

// --- HELPERS ---

/// Floating banner used for transient feedback: undo results, AI errors,
/// permission rejections from the Rust safety layer.
private struct ToastBanner: View {
    let message: String
    let isError: Bool

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: isError ? "exclamationmark.triangle.fill" : "arrow.uturn.backward.circle.fill")
                .foregroundColor(isError ? WarpTheme.warning : WarpTheme.accent)
            Text(message)
                .font(.system(size: 13, weight: .medium))
                .foregroundColor(WarpTheme.textPrimary)
                .lineLimit(2)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(
            RoundedRectangle(cornerRadius: 10)
                .fill(WarpTheme.surfacePrimary)
                .shadow(color: .black.opacity(0.3), radius: 12, y: 6)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 10)
                .stroke(isError ? WarpTheme.warning.opacity(0.4) : WarpTheme.divider, lineWidth: 1)
        )
        .frame(maxWidth: 480)
        .accessibilityIdentifier("toastBanner")
    }
}

struct VisualEffectView: NSViewRepresentable {
    let material: NSVisualEffectView.Material
    let blendingMode: NSVisualEffectView.BlendingMode

    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = material
        view.blendingMode = blendingMode
        view.state = .active
        return view
    }

    func updateNSView(_ view: NSVisualEffectView, context: Context) {
        view.material = material
        view.blendingMode = blendingMode
    }
}

// --- QUICK LOOK SUPPORT ---

class QuickLookController: NSObject, QLPreviewPanelDataSource, QLPreviewPanelDelegate {
    static let shared = QuickLookController()

    var previewURL: URL?

    func togglePreview(for path: String?) {
        guard let path = path else {
            print("Quick Look: No file path provided")
            return
        }
        previewURL = URL(fileURLWithPath: path)

        guard let panel = QLPreviewPanel.shared() else {
            print("Quick Look: Could not get panel")
            return
        }

        if panel.isVisible {
            panel.orderOut(nil)
        } else {
            panel.dataSource = self
            panel.delegate = self
            panel.reloadData()
            panel.makeKeyAndOrderFront(nil)
        }
    }

    // MARK: - QLPreviewPanelDataSource

    func numberOfPreviewItems(in panel: QLPreviewPanel!) -> Int {
        return previewURL != nil ? 1 : 0
    }

    func previewPanel(_ panel: QLPreviewPanel!, previewItemAt index: Int) -> (any QLPreviewItem)! {
        guard let url = previewURL else { return nil }
        return PreviewItem(url: url)
    }
}

// QLPreviewItem wrapper (must be NSObject subclass)
class PreviewItem: NSObject, QLPreviewItem {
    let url: URL

    init(url: URL) {
        self.url = url
        super.init()
    }

    var previewItemURL: URL? { url }
}

// NSView that accepts Quick Look panel
class QuickLookHostView: NSView {
    override var acceptsFirstResponder: Bool { true }

    override func acceptsPreviewPanelControl(_ panel: QLPreviewPanel!) -> Bool {
        return true
    }

    override func beginPreviewPanelControl(_ panel: QLPreviewPanel!) {
        panel.dataSource = QuickLookController.shared
        panel.delegate = QuickLookController.shared
    }

    override func endPreviewPanelControl(_ panel: QLPreviewPanel!) {
    }
}

// SwiftUI wrapper for Quick Look host
struct QuickLookHost: NSViewRepresentable {
    func makeNSView(context: Context) -> QuickLookHostView {
        return QuickLookHostView()
    }

    func updateNSView(_ nsView: QuickLookHostView, context: Context) {}
}

// Rename sheet view
struct RenameSheet: View {
    @State var currentName: String
    let onRename: (String) -> Void
    let onCancel: () -> Void

    var body: some View {
        VStack(spacing: 16) {
            Text("Rename")
                .font(.headline)
                .foregroundColor(WarpTheme.textPrimary)

            TextField("Name", text: $currentName)
                .textFieldStyle(.roundedBorder)
                .frame(width: 300)

            HStack {
                Button("Cancel") {
                    onCancel()
                }
                .keyboardShortcut(.escape)

                Button("Rename") {
                    onRename(currentName)
                }
                .keyboardShortcut(.return)
                .buttonStyle(.borderedProminent)
                .tint(Color(WarpTheme.accent))
            }
        }
        .padding(24)
        .background(WarpTheme.surfacePrimary)
    }
}

// MARK: - Custom Table Row View (dark theme, rounded selection)

class WarpTableRowView: NSTableRowView {
    override func drawSelection(in dirtyRect: NSRect) {
        guard isSelected else { return }
        let insetRect = bounds.insetBy(dx: 4, dy: 1)
        let path = NSBezierPath(roundedRect: insetRect, xRadius: 6, yRadius: 6)
        WarpTheme.nsSurfaceSelected.setFill()
        path.fill()
    }

    override var isEmphasized: Bool {
        get { false }
        set { }
    }

    override var interiorBackgroundStyle: NSView.BackgroundStyle {
        return .normal
    }
}

// MARK: - NSOutlineView for Expandable Folders in File List

struct FileOutlineView: NSViewRepresentable {
    let files: [SearchResult]
    @Binding var selection: Set<String>
    let loadedFolderContents: [String: [SearchResult]]
    @Binding var lastExpandedPath: String?
    /// Header text for the date column — names the field currently in force,
    /// so the list never shows an unlabelled date whose meaning you can't tell.
    let dateColumnTitle: String
    let onFolderExpanded: (String) -> Void
    let onDoubleClick: (String) -> Void

    func makeNSView(context: Context) -> NSScrollView {
        let scrollView = NSScrollView()
        let outlineView = NSOutlineView()

        scrollView.documentView = outlineView
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = false
        scrollView.autohidesScrollers = true
        scrollView.scrollerStyle = .overlay
        scrollView.drawsBackground = false

        outlineView.style = .inset
        outlineView.usesAlternatingRowBackgroundColors = false
        outlineView.allowsMultipleSelection = true
        outlineView.rowHeight = WarpTheme.fileRowHeight
        outlineView.backgroundColor = WarpTheme.nsSurfacePrimary

        let nameColumn = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("name"))
        nameColumn.title = "Name"
        nameColumn.width = 300
        nameColumn.minWidth = 200
        outlineView.addTableColumn(nameColumn)

        let sizeColumn = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("size"))
        sizeColumn.title = "Size"
        sizeColumn.width = 80
        sizeColumn.minWidth = 60
        outlineView.addTableColumn(sizeColumn)

        let dateColumn = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("date"))
        dateColumn.title = "Date"
        dateColumn.width = 120
        dateColumn.minWidth = 80
        outlineView.addTableColumn(dateColumn)

        outlineView.outlineTableColumn = nameColumn
        outlineView.delegate = context.coordinator
        outlineView.dataSource = context.coordinator
        outlineView.target = context.coordinator
        outlineView.doubleAction = #selector(Coordinator.outlineViewDoubleClick(_:))
        outlineView.setDraggingSourceOperationMask(.every, forLocal: false)
        outlineView.registerForDraggedTypes([.fileURL])

        // Style header
        outlineView.headerView?.wantsLayer = true
        outlineView.headerView?.layer?.backgroundColor = WarpTheme.nsSurfacePrimary.cgColor

        context.coordinator.outlineView = outlineView
        return scrollView
    }

    func updateNSView(_ scrollView: NSScrollView, context: Context) {
        guard let outlineView = scrollView.documentView as? NSOutlineView else { return }

        if let dateColumn = outlineView.tableColumn(withIdentifier: NSUserInterfaceItemIdentifier("date")),
           dateColumn.title != dateColumnTitle {
            dateColumn.title = dateColumnTitle
            outlineView.headerView?.needsDisplay = true
        }

        let rootChanged = context.coordinator.files.map(\.filePath) != files.map(\.filePath)
        context.coordinator.files = files
        context.coordinator.loadedFolderContents = loadedFolderContents
        context.coordinator.selection = selection
        context.coordinator.onFolderExpanded = onFolderExpanded
        context.coordinator.onDoubleClick = onDoubleClick

        if rootChanged {
            outlineView.reloadData()
        }

        // If we just loaded a folder's contents, reload that item so children appear and keep it expanded
        if let path = lastExpandedPath {
            outlineView.reloadItem(path, reloadChildren: true)
            outlineView.expandItem(path)
            lastExpandedPath = nil
        }

        // Sync selection by path
        var indexSet = IndexSet()
        for i in 0..<outlineView.numberOfRows {
            if let item = outlineView.item(atRow: i) as? String, selection.contains(item) {
                indexSet.insert(i)
            }
        }
        if outlineView.selectedRowIndexes != indexSet {
            outlineView.selectRowIndexes(indexSet, byExtendingSelection: false)
        }
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(
            files: files,
            loadedFolderContents: loadedFolderContents,
            selection: selection,
            onFolderExpanded: onFolderExpanded,
            onDoubleClick: onDoubleClick,
            onSelectionChange: { selection = $0 }
        )
    }

    class Coordinator: NSObject, NSOutlineViewDataSource, NSOutlineViewDelegate {
        var files: [SearchResult]
        var loadedFolderContents: [String: [SearchResult]]
        var selection: Set<String>
        var onFolderExpanded: (String) -> Void
        var onDoubleClick: (String) -> Void
        var onSelectionChange: (Set<String>) -> Void
        weak var outlineView: NSOutlineView?
        var iconCache: [String: NSImage] = [:]
        var thumbnailCache: [String: NSImage] = [:]

        private static let thumbnailTypes: Set<String> = [
            "jpg", "jpeg", "png", "gif", "heic", "pdf", "mp4", "mov", "psd"
        ]

        init(files: [SearchResult], loadedFolderContents: [String: [SearchResult]], selection: Set<String>, onFolderExpanded: @escaping (String) -> Void, onDoubleClick: @escaping (String) -> Void, onSelectionChange: @escaping (Set<String>) -> Void) {
            self.files = files
            self.loadedFolderContents = loadedFolderContents
            self.selection = selection
            self.onFolderExpanded = onFolderExpanded
            self.onDoubleClick = onDoubleClick
            self.onSelectionChange = onSelectionChange
        }

        func searchResult(forPath path: String) -> SearchResult? {
            files.first { $0.filePath == path } ?? loadedFolderContents.values.flatMap { $0 }.first { $0.filePath == path }
        }

        // MARK: - NSOutlineViewDataSource

        func outlineView(_ outlineView: NSOutlineView, numberOfChildrenOfItem item: Any?) -> Int {
            if item == nil {
                return files.count
            }
            guard let path = item as? String else { return 0 }
            guard let result = searchResult(forPath: path), result.isFolder else { return 0 }
            return loadedFolderContents[path]?.count ?? 0
        }

        func outlineView(_ outlineView: NSOutlineView, child index: Int, ofItem item: Any?) -> Any {
            if item == nil {
                return files[index].filePath
            }
            guard let path = item as? String, let children = loadedFolderContents[path], index < children.count else {
                return "" as String
            }
            return children[index].filePath
        }

        func outlineView(_ outlineView: NSOutlineView, isItemExpandable item: Any) -> Bool {
            guard let path = item as? String else { return false }
            return searchResult(forPath: path)?.isFolder ?? false
        }

        // MARK: - Row View (custom selection)

        func outlineView(_ outlineView: NSOutlineView, rowViewForItem item: Any) -> NSTableRowView? {
            return WarpTableRowView()
        }

        // MARK: - NSOutlineViewDelegate (expand = load children)

        func outlineView(_ outlineView: NSOutlineView, shouldExpandItem item: Any) -> Bool {
            guard let path = item as? String else { return false }
            onFolderExpanded(path)
            return true
        }

        func outlineView(_ outlineView: NSOutlineView, viewFor tableColumn: NSTableColumn?, item: Any) -> NSView? {
            guard let path = item as? String, let file = searchResult(forPath: path) else { return nil }
            let columnId = tableColumn?.identifier ?? NSUserInterfaceItemIdentifier("")

            var cellView = outlineView.makeView(withIdentifier: columnId, owner: self) as? NSTableCellView
            if cellView == nil {
                cellView = NSTableCellView()
                cellView?.identifier = columnId
                if columnId.rawValue == "name" {
                    let stack = NSStackView()
                    stack.orientation = .horizontal
                    stack.spacing = 8
                    let img = NSImageView()
                    img.imageScaling = .scaleProportionallyUpOrDown
                    img.setContentHuggingPriority(.required, for: .horizontal)
                    img.translatesAutoresizingMaskIntoConstraints = false
                    NSLayoutConstraint.activate([
                        img.widthAnchor.constraint(equalToConstant: WarpTheme.iconSize),
                        img.heightAnchor.constraint(equalToConstant: WarpTheme.iconSize)
                    ])
                    let txt = NSTextField()
                    txt.isBordered = false
                    txt.drawsBackground = false
                    txt.lineBreakMode = .byTruncatingTail
                    txt.textColor = WarpTheme.nsTextPrimary
                    txt.font = NSFont.systemFont(ofSize: 13)
                    stack.addArrangedSubview(img)
                    stack.addArrangedSubview(txt)
                    cellView?.addSubview(stack)
                    stack.translatesAutoresizingMaskIntoConstraints = false
                    NSLayoutConstraint.activate([
                        stack.leadingAnchor.constraint(equalTo: cellView!.leadingAnchor, constant: 4),
                        stack.trailingAnchor.constraint(equalTo: cellView!.trailingAnchor, constant: -4),
                        stack.centerYAnchor.constraint(equalTo: cellView!.centerYAnchor)
                    ])
                    cellView?.imageView = img
                    cellView?.textField = txt
                } else {
                    let txt = NSTextField()
                    txt.isBordered = false
                    txt.drawsBackground = false
                    txt.textColor = WarpTheme.nsTextSecondary
                    txt.font = NSFont.systemFont(ofSize: 12)
                    cellView?.addSubview(txt)
                    txt.translatesAutoresizingMaskIntoConstraints = false
                    NSLayoutConstraint.activate([
                        txt.leadingAnchor.constraint(equalTo: cellView!.leadingAnchor, constant: 4),
                        txt.centerYAnchor.constraint(equalTo: cellView!.centerYAnchor),
                        txt.trailingAnchor.constraint(equalTo: cellView!.trailingAnchor, constant: -4)
                    ])
                    cellView?.textField = txt
                }
            }

            if columnId.rawValue == "name" {
                loadIcon(for: path, into: cellView)
                cellView?.textField?.stringValue = file.fileName
                cellView?.textField?.textColor = WarpTheme.nsTextPrimary
            } else if columnId.rawValue == "size" {
                cellView?.textField?.stringValue = formatFileSize(file.fileSize)
                cellView?.textField?.textColor = WarpTheme.nsTextSecondary
            } else if columnId.rawValue == "date" {
                cellView?.textField?.stringValue = file.prettyDate
                cellView?.textField?.textColor = WarpTheme.nsTextSecondary
            }
            return cellView
        }

        // MARK: - Icon & Thumbnail Loading

        private func loadIcon(for path: String, into cellView: NSTableCellView?) {
            // Check thumbnail cache first
            if let thumb = thumbnailCache[path] {
                cellView?.imageView?.image = thumb
                return
            }
            // Check icon cache
            if let cached = iconCache[path] {
                cellView?.imageView?.image = cached
                return
            }

            // Placeholder
            cellView?.imageView?.image = NSImage(systemSymbolName: "doc", accessibilityDescription: nil)

            // Check if this file type supports thumbnails
            let ext = (path as NSString).pathExtension.lowercased()
            if Coordinator.thumbnailTypes.contains(ext) {
                loadThumbnail(for: path, into: cellView)
            } else {
                DispatchQueue.global(qos: .userInitiated).async { [weak self, weak cellView] in
                    let icon = NSWorkspace.shared.icon(forFile: path)
                    DispatchQueue.main.async {
                        self?.iconCache[path] = icon
                        cellView?.imageView?.image = icon
                    }
                }
            }
        }

        private func loadThumbnail(for path: String, into cellView: NSTableCellView?) {
            let url = URL(fileURLWithPath: path)
            let size = CGSize(width: WarpTheme.iconSize * 2, height: WarpTheme.iconSize * 2)
            let request = QLThumbnailGenerator.Request(
                fileAt: url,
                size: size,
                scale: NSScreen.main?.backingScaleFactor ?? 2.0,
                representationTypes: .thumbnail
            )

            QLThumbnailGenerator.shared.generateRepresentations(for: request) { [weak self, weak cellView] thumbnail, _, error in
                DispatchQueue.main.async {
                    if let cgImage = thumbnail?.cgImage {
                        let nsImage = NSImage(cgImage: cgImage, size: NSSize(width: WarpTheme.iconSize, height: WarpTheme.iconSize))
                        self?.thumbnailCache[path] = nsImage
                        cellView?.imageView?.image = nsImage
                    } else {
                        // Fall back to workspace icon
                        let icon = NSWorkspace.shared.icon(forFile: path)
                        self?.iconCache[path] = icon
                        cellView?.imageView?.image = icon
                    }
                }
            }
        }

        func formatFileSize(_ bytes: UInt64) -> String {
            if bytes < 1024 { return "\(bytes) B" }
            let kb = Double(bytes) / 1024
            if kb < 1024 { return String(format: "%.1f KB", kb) }
            return String(format: "%.1f MB", kb / 1024)
        }

        func outlineViewSelectionDidChange(_ notification: Notification) {
            guard let ov = notification.object as? NSOutlineView else { return }
            var newSelection = Set<String>()
            for i in ov.selectedRowIndexes {
                if let item = ov.item(atRow: i) as? String {
                    newSelection.insert(item)
                }
            }
            onSelectionChange(newSelection)
        }

        @objc func outlineViewDoubleClick(_ sender: NSOutlineView) {
            let row = sender.clickedRow
            guard row >= 0, let item = sender.item(atRow: row) as? String else { return }
            onDoubleClick(item)
        }

        func outlineView(_ outlineView: NSOutlineView, pasteboardWriterForRow row: Int) -> NSPasteboardWriting? {
            guard let item = outlineView.item(atRow: row) as? String else { return nil }
            return NSURL(fileURLWithPath: item)
        }
    }
}

// MARK: - NSTableView Wrapper for Double-Click Support (kept for reference; use FileOutlineView for main list)

struct FileTableView: NSViewRepresentable {
    let files: [SearchResult]
    @Binding var selection: Set<String>
    let onDoubleClick: (String) -> Void
    let onContextMenu: (Set<String>) -> Void

    func makeNSView(context: Context) -> NSScrollView {
        let scrollView = NSScrollView()
        let tableView = NSTableView()

        scrollView.documentView = tableView
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = false
        scrollView.autohidesScrollers = true
        scrollView.scrollerStyle = .overlay
        scrollView.drawsBackground = false

        tableView.style = .inset
        tableView.usesAlternatingRowBackgroundColors = false
        tableView.allowsMultipleSelection = true
        tableView.allowsColumnReordering = false
        tableView.rowHeight = WarpTheme.fileRowHeight
        tableView.backgroundColor = WarpTheme.nsSurfacePrimary

        let nameColumn = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("name"))
        nameColumn.title = "Name"
        nameColumn.width = 300
        nameColumn.minWidth = 200
        tableView.addTableColumn(nameColumn)

        let sizeColumn = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("size"))
        sizeColumn.title = "Size"
        sizeColumn.width = 80
        sizeColumn.minWidth = 60
        tableView.addTableColumn(sizeColumn)

        let dateColumn = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("date"))
        dateColumn.title = "Date"
        dateColumn.width = 120
        dateColumn.minWidth = 80
        tableView.addTableColumn(dateColumn)

        tableView.delegate = context.coordinator
        tableView.dataSource = context.coordinator
        tableView.target = context.coordinator
        tableView.doubleAction = #selector(Coordinator.tableViewDoubleClick(_:))
        tableView.setDraggingSourceOperationMask(.every, forLocal: false)
        tableView.registerForDraggedTypes([.fileURL])

        context.coordinator.tableView = tableView

        return scrollView
    }

    func updateNSView(_ scrollView: NSScrollView, context: Context) {
        guard let tableView = scrollView.documentView as? NSTableView else { return }

        let filesChanged = context.coordinator.files.count != files.count ||
            !zip(context.coordinator.files, files).allSatisfy { $0.filePath == $1.filePath }

        context.coordinator.files = files
        context.coordinator.selection = selection
        context.coordinator.onDoubleClick = onDoubleClick

        if filesChanged {
            tableView.reloadData()
        }

        let currentSelection = tableView.selectedRowIndexes
        var newIndexes = IndexSet()
        for (index, file) in files.enumerated() {
            if selection.contains(file.filePath) {
                newIndexes.insert(index)
            }
        }

        if currentSelection != newIndexes {
            tableView.selectRowIndexes(newIndexes, byExtendingSelection: false)
        }
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(files: files, selection: selection, onDoubleClick: onDoubleClick, onSelectionChange: { newSelection in
            DispatchQueue.main.async {
                self.selection = newSelection
            }
        })
    }

    class Coordinator: NSObject, NSTableViewDelegate, NSTableViewDataSource {
        var files: [SearchResult]
        var selection: Set<String>
        var onDoubleClick: (String) -> Void
        var onSelectionChange: (Set<String>) -> Void
        weak var tableView: NSTableView?
        var iconCache: [String: NSImage] = [:]

        init(files: [SearchResult], selection: Set<String>, onDoubleClick: @escaping (String) -> Void, onSelectionChange: @escaping (Set<String>) -> Void) {
            self.files = files
            self.selection = selection
            self.onDoubleClick = onDoubleClick
            self.onSelectionChange = onSelectionChange
        }

        func numberOfRows(in tableView: NSTableView) -> Int {
            return files.count
        }

        func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
            guard row < files.count else { return nil }
            let file = files[row]
            let columnId = tableColumn?.identifier ?? NSUserInterfaceItemIdentifier("")

            var cellView = tableView.makeView(withIdentifier: columnId, owner: self) as? NSTableCellView

            if cellView == nil {
                cellView = NSTableCellView()
                cellView?.identifier = columnId

                if columnId.rawValue == "name" {
                    let stack = NSStackView()
                    stack.orientation = .horizontal
                    stack.spacing = 8

                    let img = NSImageView()
                    img.imageScaling = .scaleProportionallyUpOrDown
                    img.setContentHuggingPriority(.required, for: .horizontal)
                    img.translatesAutoresizingMaskIntoConstraints = false
                    NSLayoutConstraint.activate([
                        img.widthAnchor.constraint(equalToConstant: WarpTheme.iconSize),
                        img.heightAnchor.constraint(equalToConstant: WarpTheme.iconSize)
                    ])

                    let txt = NSTextField()
                    txt.isBordered = false
                    txt.drawsBackground = false
                    txt.lineBreakMode = .byTruncatingTail
                    txt.textColor = WarpTheme.nsTextPrimary
                    txt.font = NSFont.systemFont(ofSize: 13)

                    stack.addArrangedSubview(img)
                    stack.addArrangedSubview(txt)
                    cellView?.addSubview(stack)

                    stack.translatesAutoresizingMaskIntoConstraints = false
                    NSLayoutConstraint.activate([
                        stack.leadingAnchor.constraint(equalTo: cellView!.leadingAnchor, constant: 4),
                        stack.trailingAnchor.constraint(equalTo: cellView!.trailingAnchor, constant: -4),
                        stack.centerYAnchor.constraint(equalTo: cellView!.centerYAnchor)
                    ])
                    cellView?.imageView = img
                    cellView?.textField = txt
                } else {
                    let txt = NSTextField()
                    txt.isBordered = false
                    txt.drawsBackground = false
                    txt.textColor = WarpTheme.nsTextSecondary
                    txt.font = NSFont.systemFont(ofSize: 12)
                    cellView?.addSubview(txt)

                    txt.translatesAutoresizingMaskIntoConstraints = false
                    NSLayoutConstraint.activate([
                        txt.leadingAnchor.constraint(equalTo: cellView!.leadingAnchor, constant: 4),
                        txt.centerYAnchor.constraint(equalTo: cellView!.centerYAnchor),
                        txt.trailingAnchor.constraint(equalTo: cellView!.trailingAnchor, constant: -4)
                    ])
                    cellView?.textField = txt
                }
            }

            if columnId.rawValue == "name" {
                let filePath = file.filePath
                if let cachedIcon = iconCache[filePath] {
                    cellView?.imageView?.image = cachedIcon
                } else {
                    cellView?.imageView?.image = NSImage(systemSymbolName: "doc", accessibilityDescription: nil)
                    DispatchQueue.global(qos: .userInitiated).async { [weak cellView] in
                        let icon = NSWorkspace.shared.icon(forFile: filePath)
                        DispatchQueue.main.async {
                            self.iconCache[filePath] = icon
                            cellView?.imageView?.image = icon
                        }
                    }
                }
                cellView?.textField?.stringValue = file.fileName
                cellView?.textField?.textColor = WarpTheme.nsTextPrimary
            } else if columnId.rawValue == "size" {
                cellView?.textField?.stringValue = formatFileSize(file.fileSize)
                cellView?.textField?.textColor = WarpTheme.nsTextSecondary
            } else if columnId.rawValue == "date" {
                cellView?.textField?.stringValue = file.prettyDate
                cellView?.textField?.textColor = WarpTheme.nsTextSecondary
            }

            return cellView
        }

        func formatFileSize(_ bytes: UInt64) -> String {
            if bytes < 1024 { return "\(bytes) B" }
            let kb = Double(bytes) / 1024
            if kb < 1024 { return String(format: "%.1f KB", kb) }
            let mb = kb / 1024
            if mb < 1024 { return String(format: "%.1f MB", mb) }
            let gb = mb / 1024
            return String(format: "%.1f GB", gb)
        }

        func tableViewSelectionDidChange(_ notification: Notification) {
            guard let tableView = notification.object as? NSTableView else { return }
            var newSelection = Set<String>()
            for index in tableView.selectedRowIndexes {
                if index < files.count {
                    newSelection.insert(files[index].filePath)
                }
            }
            onSelectionChange(newSelection)
        }

        @objc func tableViewDoubleClick(_ sender: NSTableView) {
            let clickedRow = sender.clickedRow
            guard clickedRow >= 0 && clickedRow < files.count else { return }
            let file = files[clickedRow]
            onDoubleClick(file.filePath)
        }

        // MARK: - Drag and Drop Support

        func tableView(_ tableView: NSTableView, pasteboardWriterForRow row: Int) -> NSPasteboardWriting? {
            guard row < files.count else { return nil }
            let file = files[row]
            return NSURL(fileURLWithPath: file.filePath)
        }

        func tableView(_ tableView: NSTableView, draggingSession session: NSDraggingSession, willBeginAt screenPoint: NSPoint, forRowIndexes rowIndexes: IndexSet) {
        }
    }
}
