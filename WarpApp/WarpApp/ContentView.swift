//
//  ContentView.swift
//  WarpApp
//
//  Created by Rohan Potta on 1/10/26.
//

import SwiftUI

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
    @State private var selectedFileId: String? = nil
    @State private var selectedSidebarItem: SidebarItem = .recents
    @State private var columnVisibility: NavigationSplitViewVisibility = .all
    @State private var sortOrder = [KeyPathComparator(\SearchResult.dateValue, order: .reverse)]

    var body: some View {
        NavigationSplitView(columnVisibility: $columnVisibility) {
            // --- SIDEBAR ---
            List(selection: $selectedSidebarItem) {
                Section("Favorites") {
                    ForEach([SidebarItem.recents, .desktop, .documents, .downloads], id: \.self) { item in
                        Label(item.displayName, systemImage: item.icon)
                            .tag(item)
                    }
                }
                
                Section("Locations") {
                    ForEach([SidebarItem.user, .applications, .trash], id: \.self) { item in
                        Label(item.displayName, systemImage: item.icon)
                            .tag(item)
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

                // --- TABLE VIEW ---
                Table(sortedResults, selection: $selectedFileId, sortOrder: $sortOrder) {
                    TableColumn("Name") { file in
                        HStack {
                            Image(nsImage: NSWorkspace.shared.icon(forFile: file.filePath))
                                .resizable()
                                .aspectRatio(contentMode: .fit)
                                .frame(width: 20, height: 20)
                            Text(file.fileName)
                                .lineLimit(1)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading) // Fill entire cell width
                        .contentShape(Rectangle()) // Make whitespace clickable
                        .gesture(TapGesture(count: 2).onEnded {
                            openFile(file.filePath)
                        })
                        .simultaneousGesture(TapGesture().onEnded {
                            // Fires immediately on first click for instant highlight
                            selectedFileId = file.filePath
                        })
                    }
                    .width(min: 200, ideal: 300)
                    
                    TableColumn("Kind", value: \.fileKind) { file in
                        Text(file.fileKind)
                            .foregroundColor(.secondary)
                    }
                    .width(min: 100, ideal: 140)
                    
                    TableColumn("Date", value: \.dateValue) { file in
                        VStack(alignment: .leading, spacing: 0) {
                            Text(formattedDate(file.dateValue))
                            Text(file.dateKind)
                                .font(.caption2)
                                .foregroundColor(.secondary)
                        }
                    }
                    .width(min: 100, ideal: 120)
                }
                .tableStyle(.inset(alternatesRowBackgrounds: true))
                .contextMenu {
                    Button("Open") {
                        if let id = selectedFileId {
                            openFile(id)
                        }
                    }
                    Button("Show in Finder") {
                        if let id = selectedFileId {
                            NSWorkspace.shared.selectFile(id, inFileViewerRootedAtPath: "")
                        }
                    }
                }
                .onKeyPress(.return) {
                    if let id = selectedFileId {
                        openFile(id)
                    }
                    return .handled
                }
            }
            .frame(minWidth: 500)
        }
        .frame(minWidth: 750, minHeight: 500)
        .task { loadFolder(.recents) }
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
                self.selectedFileId = contents.first?.filePath
            }
        }
    }
    
    func loadRecents() {
        Task {
            let recentFiles = await Task.detached(priority: .userInitiated) {
                return getRecentFiles()
            }.value
            
            await MainActor.run {
                if self.query.isEmpty {
                    self.results = recentFiles
                    self.selectedFileId = recentFiles.first?.filePath
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
                self.selectedFileId = newResults.first?.filePath
            }
        }
    }
    
    func openSelected() {
        if let selectedFileId = selectedFileId {
            openFile(selectedFileId)
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
