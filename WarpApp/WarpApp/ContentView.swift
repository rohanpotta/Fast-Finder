//
//  ContentView.swift
//  WarpApp
//
//  Created by Rohan Potta on 1/10/26.
//

import SwiftUI
import Quartz // For Quick Look preview

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
    @State private var columnVisibility: NavigationSplitViewVisibility = .all
    @State private var sortOrder = [KeyPathComparator(\SearchResult.dateValue, order: .reverse)]
    
    // File operation state
    @State private var showRenameSheet = false
    @State private var renameText = ""
    @State private var showMovePanel = false
    
    // AI state
    @State private var showAIBar = false
    @State private var aiPrefilledPlan: AIActionPlan?
    
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

    var body: some View {
        NavigationSplitView(columnVisibility: $columnVisibility) {
            // --- SIDEBAR (flat Favorites / Locations) ---
            List(selection: $selectedSidebarItem) {
                Section("Favorites") {
                    ForEach([SidebarItem.recents, .desktop, .documents, .downloads], id: \.self) { item in
                        Label(item.displayName, systemImage: item.icon)
                            .tag(item)
                            .onDrop(of: [.fileURL], isTargeted: nil) { providers in
                                handleDrop(providers: providers, to: item)
                            }
                    }
                }
                Section("Locations") {
                    ForEach([SidebarItem.user, .applications, .trash], id: \.self) { item in
                        Label(item.displayName, systemImage: item.icon)
                            .tag(item)
                            .onDrop(of: [.fileURL], isTargeted: nil) { providers in
                                handleDrop(providers: providers, to: item)
                            }
                    }
                }
            }
            .listStyle(.sidebar)
            .frame(minWidth: 180)
            .onChange(of: selectedSidebarItem) { newItem in
                navigationPathStack = []
                loadedFolderContents.removeAll()
                loadFolder(newItem)
            }
        } detail: {
            // --- MAIN CONTENT AREA ---
            VStack(spacing: 0) {
                // Back button when drilled into a subfolder
                if !navigationPathStack.isEmpty {
                    HStack(spacing: 8) {
                        Button(action: goBack) {
                            Image(systemName: "chevron.left")
                            Text("Back")
                        }
                        .buttonStyle(.plain)
                        Text((navigationPathStack.last as NSString?)?.lastPathComponent ?? "")
                            .font(.subheadline)
                            .foregroundColor(.secondary)
                            .lineLimit(1)
                        Spacer()
                    }
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .background(Color(nsColor: .controlBackgroundColor).opacity(0.5))
                    Divider()
                }
                
                // Search Bar
                HStack {
                    Image(systemName: "magnifyingglass")
                        .foregroundColor(.gray)
                        .font(.title2)
                    
                    TextField("Search...", text: $query)
                        .textFieldStyle(.plain)
                        .font(.title2)
                        .padding(.vertical, 12)
                        .onSubmit { openSelected() }
                        .onChange(of: query) { newValue in
                            runSearch(for: newValue)
                        }
                }
                .padding(.horizontal)
                .background(Color(nsColor: .controlBackgroundColor).opacity(0.5))
                
                Divider()

                // --- OUTLINE VIEW (arrow = expand folder inline; double-click = go into folder) ---
                FileOutlineView(
                    files: sortedResults,
                    selection: $selectedFileIds,
                    loadedFolderContents: loadedFolderContents,
                    lastExpandedPath: $lastExpandedPath,
                    onFolderExpanded: { path in
                        guard loadedFolderContents[path] == nil else { return }
                        Task {
                            let contents = await Task.detached(priority: .userInitiated) {
                                loadDirectoryContents(path: path, showHidden: false)
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
                            navigateIntoFolder(path)
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
                            if let path = selectedFileIds.first,
                               let name = URL(fileURLWithPath: path).lastPathComponent.components(separatedBy: ".").first {
                                renameText = URL(fileURLWithPath: path).lastPathComponent
                                showRenameSheet = true
                            }
                        }
                    }
                    
                    Divider()
                    
                    Button("Move to Trash") {
                        let paths = Array(selectedFileIds)
                        let result = trashFiles(paths: paths)
                        if result.success {
                            results.removeAll { selectedFileIds.contains($0.filePath) }
                            selectedFileIds.removeAll()
                        }
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
                    let result = trashFiles(paths: paths)
                    if result.success {
                        results.removeAll { selectedFileIds.contains($0.filePath) }
                        selectedFileIds.removeAll()
                    }
                    return .handled
                }
            }
            .frame(minWidth: 500)
        }
        .frame(minWidth: 750, minHeight: 500)
        .background(QuickLookHost())
        .task { loadFolder(.recents) }
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
        // Cmd+K to toggle AI bar - using background button for keyboard shortcut
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
    
    // --- ACTIONS ---
    
    func executeAIPlan(_ plan: AIActionPlan, userQuery: String) {
        // Handle search: run search, then re-call AI with results so it can return moveFiles/trash/etc.
        if plan.action == .search {
            let searchQuery = plan.searchQuery ?? userQuery
            print("🔍 AI searching for: \(searchQuery)")
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
                        if newPlan.action == .search {
                            // Still no files found; show the plan so user sees the message
                            aiPrefilledPlan = newPlan
                        } else {
                            aiPrefilledPlan = newPlan
                        }
                    }
                } catch {
                    await MainActor.run {
                        aiPrefilledPlan = AIActionPlan(
                            action: .unknown,
                            sourcePaths: nil,
                            destination: nil,
                            searchQuery: nil,
                            explanation: "Search completed but couldn’t interpret next step: \(error.localizedDescription)"
                        )
                    }
                }
            }
            return
        }
        
        // For file operations, we need source paths
        guard let paths = plan.sourcePaths, !paths.isEmpty else {
            print("⚠️ AI returned no files to act on: \(plan.explanation)")
            return
        }
        
        switch plan.action {
        case .moveFiles:
            if let dest = plan.destination {
                let result = moveFiles(sourcePaths: paths, destination: dest)
                if result.success {
                    print("✅ AI moved \(paths.count) files to \(dest)")
                    refreshCurrentContent()
                } else {
                    print("❌ Move failed: \(result.message)")
                }
            }
        case .trashFiles:
            let result = trashFiles(paths: paths)
            if result.success {
                print("✅ AI trashed \(paths.count) files")
                results.removeAll { paths.contains($0.filePath) }
                selectedFileIds.removeAll()
            } else {
                print("❌ Trash failed: \(result.message)")
            }
        case .compressFiles:
            let firstPath = paths[0] as NSString
            let parentDir = firstPath.deletingLastPathComponent
            let archivePath = "\(parentDir)/AI_Archive_\(Int(Date().timeIntervalSince1970)).zip"
            
            let result = compressFiles(paths: paths, archivePath: archivePath)
            if result.success {
                print("✅ AI compressed \(paths.count) files to \(archivePath)")
                refreshCurrentContent()
            } else {
                print("❌ Compress failed: \(result.message)")
            }
        case .search, .unknown:
            print("⚠️ Unhandled action: \(plan.action)")
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
        Task {
            let contents = await Task.detached(priority: .userInitiated) {
                return loadDirectoryContents(path: path, showHidden: showHidden)
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
        navigationPathStack.removeLast()
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
        // INSTANT: Load cached data first for immediate display
        Task {
            let cachedFiles = await Task.detached(priority: .userInitiated) {
                return loadCachedIndex()
            }.value
            
            await MainActor.run {
                if self.query.isEmpty && !cachedFiles.isEmpty {
                    // Filter to last 7 days and show immediately
                    let weekAgo = Date().timeIntervalSince1970 - (60 * 60 * 24 * 7)
                    let recent = cachedFiles
                        .filter { Double($0.dateValue) > weekAgo }
                        .sorted { $0.dateValue > $1.dateValue }
                        .prefix(50)
                    self.results = Array(recent)
                    self.selectedFileIds = self.results.first.map { [$0.filePath] } ?? []
                }
            }
            
            // BACKGROUND: Rebuild index for fresh data
            let freshFiles = await Task.detached(priority: .background) {
                return rebuildIndex()
            }.value
            
            await MainActor.run {
                if self.query.isEmpty && self.selectedSidebarItem == .recents {
                    let weekAgo = Date().timeIntervalSince1970 - (60 * 60 * 24 * 7)
                    let recent = freshFiles
                        .filter { Double($0.dateValue) > weekAgo }
                        .sorted { $0.dateValue > $1.dateValue }
                        .prefix(50)
                    self.results = Array(recent)
                    self.selectedFileIds = self.results.first.map { [$0.filePath] } ?? []
                }
            }
        }
    }
    
    func loadDirectoryContents(path: String, showHidden: Bool = false) -> [SearchResult] {
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
                    
                    // Get all three date types
                    let modDate = (attrs?[.modificationDate] as? Date) ?? Date.distantPast
                    let createDate = (attrs?[.creationDate] as? Date) ?? Date.distantPast
                    // Access time requires lower-level API, use mod date as fallback
                    
                    // Find the most recent date
                    let (bestDate, dateKind): (Date, String) = {
                        if createDate > modDate {
                            return (createDate, "Created")
                        }
                        return (modDate, "Modified")
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
        return await Task.detached(priority: .userInitiated) {
            searchFiles(query: text)
        }.value
    }
    
    func openSelected() {
        let path = selectedFileIds.first ?? results.first?.filePath
        guard let path = path else { return }
        let isFolder = results.first { $0.filePath == path }?.isFolder ?? false
        if isFolder {
            navigateIntoFolder(path)
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
            // Refresh to show the new archive
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
                // Move to trash
                let result = trashFiles(paths: droppedPaths)
                if result.success {
                    self.results.removeAll { droppedPaths.contains($0.filePath) }
                    self.selectedFileIds.removeAll()
                }
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
            // Set data source BEFORE showing
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
        // Clean up if needed
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
            }
        }
        .padding(24)
    }
}

// MARK: - NSOutlineView for Expandable Folders in File List

struct FileOutlineView: NSViewRepresentable {
    let files: [SearchResult]
    @Binding var selection: Set<String>
    let loadedFolderContents: [String: [SearchResult]]
    @Binding var lastExpandedPath: String?
    let onFolderExpanded: (String) -> Void
    let onDoubleClick: (String) -> Void
    
    func makeNSView(context: Context) -> NSScrollView {
        let scrollView = NSScrollView()
        let outlineView = NSOutlineView()
        
        scrollView.documentView = outlineView
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = false
        scrollView.autohidesScrollers = true
        
        outlineView.style = .inset
        outlineView.usesAlternatingRowBackgroundColors = true
        outlineView.allowsMultipleSelection = true
        outlineView.rowHeight = 24
        
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
        
        context.coordinator.outlineView = outlineView
        return scrollView
    }
    
    func updateNSView(_ scrollView: NSScrollView, context: Context) {
        guard let outlineView = scrollView.documentView as? NSOutlineView else { return }
        
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
        
        // MARK: - NSOutlineViewDelegate (expand = load children)
        
        /// Called when user clicks the disclosure arrow; we get the item (path) directly and trigger load.
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
                    stack.spacing = 6
                    let img = NSImageView()
                    img.imageScaling = .scaleProportionallyUpOrDown
                    img.setContentHuggingPriority(.required, for: .horizontal)
                    img.translatesAutoresizingMaskIntoConstraints = false
                    NSLayoutConstraint.activate([img.widthAnchor.constraint(equalToConstant: 16), img.heightAnchor.constraint(equalToConstant: 16)])
                    let txt = NSTextField()
                    txt.isBordered = false
                    txt.drawsBackground = false
                    txt.lineBreakMode = .byTruncatingTail
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
                    txt.textColor = .secondaryLabelColor
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
                if let cached = iconCache[path] {
                    cellView?.imageView?.image = cached
                } else {
                    cellView?.imageView?.image = NSImage(systemSymbolName: "doc", accessibilityDescription: nil)
                    DispatchQueue.global(qos: .userInitiated).async { [weak self, weak cellView] in
                        let icon = NSWorkspace.shared.icon(forFile: path)
                        DispatchQueue.main.async {
                            self?.iconCache[path] = icon
                            cellView?.imageView?.image = icon
                        }
                    }
                }
                cellView?.textField?.stringValue = file.fileName
            } else if columnId.rawValue == "size" {
                cellView?.textField?.stringValue = formatFileSize(file.fileSize)
            } else if columnId.rawValue == "date" {
                cellView?.textField?.stringValue = file.prettyDate
            }
            return cellView
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
        
        // Configure scroll view
        scrollView.documentView = tableView
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = false
        scrollView.autohidesScrollers = true
        
        // Configure table view
        tableView.style = .inset
        tableView.usesAlternatingRowBackgroundColors = true
        tableView.allowsMultipleSelection = true
        tableView.allowsColumnReordering = false
        tableView.rowHeight = 24
        
        // Create columns
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
        
        // Set up delegate and data source
        tableView.delegate = context.coordinator
        tableView.dataSource = context.coordinator
        
        // CRITICAL: Set double-click action
        tableView.target = context.coordinator
        tableView.doubleAction = #selector(Coordinator.tableViewDoubleClick(_:))
        
        // Enable drag and drop (use .every for move/copy/delete support)
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
        
        // Only reload if files actually changed (not on selection change)
        if filesChanged {
            tableView.reloadData()
        }
        
        // Sync selection from SwiftUI to NSTableView (without triggering delegate)
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
        var iconCache: [String: NSImage] = [:]  // Cache icons to avoid repeated disk access
        
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
            
            // 1. TRY TO RECYCLE AN EXISTING CELL
            var cellView = tableView.makeView(withIdentifier: columnId, owner: self) as? NSTableCellView
            
            // 2. IF NO RECYCLABLE CELL EXISTS, CREATE ONE
            if cellView == nil {
                cellView = NSTableCellView()
                cellView?.identifier = columnId
                
                if columnId.rawValue == "name" {
                    let stack = NSStackView()
                    stack.orientation = .horizontal
                    stack.spacing = 6
                    
                    let img = NSImageView()
                    img.imageScaling = .scaleProportionallyUpOrDown
                    img.setContentHuggingPriority(.required, for: .horizontal)
                    img.translatesAutoresizingMaskIntoConstraints = false
                    NSLayoutConstraint.activate([
                        img.widthAnchor.constraint(equalToConstant: 16),
                        img.heightAnchor.constraint(equalToConstant: 16)
                    ])
                    
                    let txt = NSTextField()
                    txt.isBordered = false
                    txt.drawsBackground = false
                    txt.lineBreakMode = .byTruncatingTail
                    
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
                    // Simple text columns (Size, Date)
                    let txt = NSTextField()
                    txt.isBordered = false
                    txt.drawsBackground = false
                    txt.textColor = .secondaryLabelColor
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
            
            // 3. POPULATE DATA (runs for both new and recycled cells)
            if columnId.rawValue == "name" {
                // Async icon loading with cache
                let filePath = file.filePath
                if let cachedIcon = iconCache[filePath] {
                    cellView?.imageView?.image = cachedIcon
                } else {
                    // Set placeholder first
                    cellView?.imageView?.image = NSImage(systemSymbolName: "doc", accessibilityDescription: nil)
                    
                    // Load async
                    DispatchQueue.global(qos: .userInitiated).async { [weak cellView] in
                        let icon = NSWorkspace.shared.icon(forFile: filePath)
                        DispatchQueue.main.async {
                            self.iconCache[filePath] = icon
                            cellView?.imageView?.image = icon
                        }
                    }
                }
                cellView?.textField?.stringValue = file.fileName
            } else if columnId.rawValue == "size" {
                cellView?.textField?.stringValue = formatFileSize(file.fileSize)
            } else if columnId.rawValue == "date" {
                // Use pre-formatted date from Rust (no formatter on main thread!)
                cellView?.textField?.stringValue = file.prettyDate
            }
            
            return cellView
        }
        
        // Helper to format file size
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
            // Drag all selected rows if the dragged row is in selection
            // Otherwise just drag the single row
        }
    }
}
