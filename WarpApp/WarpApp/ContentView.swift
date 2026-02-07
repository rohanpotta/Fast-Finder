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

    var body: some View {
        NavigationSplitView(columnVisibility: $columnVisibility) {
            // --- SIDEBAR ---
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
                loadFolder(newItem)
            }
        } detail: {
            // --- MAIN CONTENT AREA ---
            VStack(spacing: 0) {
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

                // --- TABLE VIEW (NSTableView for double-click support) ---
                FileTableView(
                    files: sortedResults,
                    selection: $selectedFileIds,
                    onDoubleClick: { path in
                        openFile(path)
                    },
                    onContextMenu: { _ in }
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
                            loadFolder(selectedSidebarItem)
                        }
                    }
                    showRenameSheet = false
                },
                onCancel: { showRenameSheet = false }
            )
        }
    }
    
    // Sorted results based on current sort order
    var sortedResults: [SearchResult] {
        results.sorted(using: sortOrder)
    }
    
    // --- ACTIONS ---
    
    func loadFolder(_ item: SidebarItem) {
        query = ""
        
        if item == .recents {
            loadRecents()
            return
        }
        
        Task {
            let folderPath = item.path
            let isTrash = (item == .trash)
            let contents = await Task.detached(priority: .userInitiated) {
                return loadDirectoryContents(path: folderPath, showHidden: isTrash)
            }.value
            
            await MainActor.run {
                self.results = contents
                self.selectedFileIds = contents.first.map { [$0.filePath] } ?? []
            }
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
                        fileKind: fileKind
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

    func runSearch(for text: String) {
        searchTask?.cancel()
        
        if text.isEmpty {
            loadFolder(selectedSidebarItem)
            return
        }
        
        searchTask = Task {
            try? await Task.sleep(nanoseconds: 100_000_000)
            if Task.isCancelled { return }
            
            let newResults = await Task.detached(priority: .userInitiated) {
                return searchFiles(query: text)
            }.value
            
            await MainActor.run {
                self.results = newResults
                self.selectedFileIds = newResults.first.map { [$0.filePath] } ?? []
            }
        }
    }
    
    func openSelected() {
        if let firstSelected = selectedFileIds.first {
            openFile(firstSelected)
        } else if let first = results.first {
            openFile(first.filePath)
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
            loadFolder(selectedSidebarItem)
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

// MARK: - NSTableView Wrapper for Double-Click Support

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
        
        let kindColumn = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("kind"))
        kindColumn.title = "Kind"
        kindColumn.width = 120
        kindColumn.minWidth = 80
        tableView.addTableColumn(kindColumn)
        
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
        
        // Enable drag and drop
        tableView.setDraggingSourceOperationMask(.copy, forLocal: false)
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
            
            let cellView = NSTableCellView()
            
            if tableColumn?.identifier.rawValue == "name" {
                let stackView = NSStackView()
                stackView.orientation = .horizontal
                stackView.spacing = 6
                
                // Icon
                let icon = NSWorkspace.shared.icon(forFile: file.filePath)
                icon.size = NSSize(width: 16, height: 16)
                let imageView = NSImageView(image: icon)
                imageView.imageScaling = .scaleProportionallyUpOrDown
                imageView.setContentHuggingPriority(.required, for: .horizontal)
                
                // Text
                let textField = NSTextField(labelWithString: file.fileName)
                textField.lineBreakMode = .byTruncatingTail
                
                stackView.addArrangedSubview(imageView)
                stackView.addArrangedSubview(textField)
                
                cellView.addSubview(stackView)
                stackView.translatesAutoresizingMaskIntoConstraints = false
                NSLayoutConstraint.activate([
                    stackView.leadingAnchor.constraint(equalTo: cellView.leadingAnchor, constant: 4),
                    stackView.trailingAnchor.constraint(equalTo: cellView.trailingAnchor, constant: -4),
                    stackView.centerYAnchor.constraint(equalTo: cellView.centerYAnchor)
                ])
            } else if tableColumn?.identifier.rawValue == "kind" {
                let textField = NSTextField(labelWithString: file.fileKind)
                textField.textColor = .secondaryLabelColor
                cellView.addSubview(textField)
                textField.translatesAutoresizingMaskIntoConstraints = false
                NSLayoutConstraint.activate([
                    textField.leadingAnchor.constraint(equalTo: cellView.leadingAnchor, constant: 4),
                    textField.centerYAnchor.constraint(equalTo: cellView.centerYAnchor)
                ])
            } else if tableColumn?.identifier.rawValue == "date" {
                let date = Date(timeIntervalSince1970: TimeInterval(file.dateValue))
                let formatter = RelativeDateTimeFormatter()
                formatter.unitsStyle = .abbreviated
                let dateStr = formatter.localizedString(for: date, relativeTo: Date())
                
                let textField = NSTextField(labelWithString: dateStr)
                textField.textColor = .secondaryLabelColor
                cellView.addSubview(textField)
                textField.translatesAutoresizingMaskIntoConstraints = false
                NSLayoutConstraint.activate([
                    textField.leadingAnchor.constraint(equalTo: cellView.leadingAnchor, constant: 4),
                    textField.centerYAnchor.constraint(equalTo: cellView.centerYAnchor)
                ])
            }
            
            return cellView
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
