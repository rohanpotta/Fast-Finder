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
    case home = "Home"
    
    var id: String { rawValue }
    
    var icon: String {
        switch self {
        case .recents: return "clock.fill"
        case .applications: return "square.grid.2x2.fill"
        case .desktop: return "desktopcomputer"
        case .documents: return "doc.fill"
        case .downloads: return "arrow.down.circle.fill"
        case .home: return "house.fill"
        }
    }
    
    var path: String {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        switch self {
        case .recents: return "" // Special case
        case .applications: return "/Applications"
        case .desktop: return "\(home)/Desktop"
        case .documents: return "\(home)/Documents"
        case .downloads: return "\(home)/Downloads"
        case .home: return home
        }
    }
}

struct ContentView: View {
    @State private var query = ""
    @State private var results: [SearchResult] = []
    @State private var searchTask: Task<Void, Never>? = nil
    @State private var selectedFileId: String? = nil
    @State private var selectedSidebarItem: SidebarItem = .recents
    @State private var columnVisibility: NavigationSplitViewVisibility = .all

    var body: some View {
        NavigationSplitView(columnVisibility: $columnVisibility) {
            // --- SIDEBAR ---
            List(selection: $selectedSidebarItem) {
                Section("Favorites") {
                    ForEach([SidebarItem.recents, .desktop, .documents, .downloads], id: \.self) { item in
                        Label(item.rawValue, systemImage: item.icon)
                            .tag(item)
                    }
                }
                
                Section("Locations") {
                    ForEach([SidebarItem.applications, .home], id: \.self) { item in
                        Label(item.rawValue, systemImage: item.icon)
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

                // Results List
                ScrollView {
                    LazyVStack(spacing: 0) {
                        ForEach(results, id: \.filePath) { file in
                            FileRowView(
                                file: file,
                                isSelected: selectedFileId == file.filePath
                            )
                            .contentShape(Rectangle())
                            .onHover { isHovering in
                                if isHovering { selectedFileId = file.filePath }
                            }
                            .onTapGesture {
                                openFile(file.filePath)
                            }
                        }
                    }
                    .padding(8)
                }
            }
            .frame(minWidth: 400)
        }
        .frame(minWidth: 700, minHeight: 500)
        .task { loadFolder(.recents) }
    }
    
    // --- ACTIONS ---
    
    func loadFolder(_ item: SidebarItem) {
        query = "" // Clear search when switching folders
        
        if item == .recents {
            loadRecents()
            return
        }
        
        // Load directory contents
        Task {
            let folderPath = item.path
            let contents = await Task.detached(priority: .userInitiated) {
                return loadDirectoryContents(path: folderPath)
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
    
    func loadDirectoryContents(path: String) -> [SearchResult] {
        var items: [(result: SearchResult, modDate: Date)] = []
        let fileManager = FileManager.default
        
        do {
            let contents = try fileManager.contentsOfDirectory(atPath: path)
            for name in contents {
                // Skip hidden files
                if name.hasPrefix(".") { continue }
                
                let fullPath = (path as NSString).appendingPathComponent(name)
                var isDir: ObjCBool = false
                
                if fileManager.fileExists(atPath: fullPath, isDirectory: &isDir) {
                    let attrs = try? fileManager.attributesOfItem(atPath: fullPath)
                    let size = (attrs?[.size] as? UInt64) ?? 0
                    let modDate = (attrs?[.modificationDate] as? Date) ?? Date.distantPast
                    
                    // Use modification timestamp as score for sorting
                    let score = Int64(modDate.timeIntervalSince1970)
                    
                    items.append((
                        result: SearchResult(
                            fileName: name,
                            filePath: fullPath,
                            fileSize: size,
                            isFolder: isDir.boolValue,
                            score: score
                        ),
                        modDate: modDate
                    ))
                }
            }
        } catch {
            print("Error loading directory: \(error)")
        }
        
        // Sort by modification date (most recent first)
        items.sort { $0.modDate > $1.modDate }
        
        return items.map { $0.result }
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
        if let selectedFileId = selectedFileId,
           let file = results.first(where: { $0.filePath == selectedFileId }) {
            openFile(file.filePath)
        } else if let first = results.first {
            openFile(first.filePath)
        }
    }

    func openFile(_ path: String) {
        let url = URL(fileURLWithPath: path)
        NSWorkspace.shared.open(url)
    }
}

// --- SUBVIEWS ---

struct FileRowView: View {
    let file: SearchResult
    let isSelected: Bool

    var body: some View {
        HStack {
            // Real macOS system icon for the file
            Image(nsImage: NSWorkspace.shared.icon(forFile: file.filePath))
                .resizable()
                .aspectRatio(contentMode: .fit)
                .frame(width: 32, height: 32)

            VStack(alignment: .leading, spacing: 2) {
                Text(file.fileName)
                    .font(.system(size: 13, weight: .medium))
                    .foregroundColor(.primary)
                
                Text(formattedSize)
                    .font(.system(size: 11))
                    .foregroundColor(.secondary)
            }
            Spacer()
        }
        .padding(.vertical, 6)
        .padding(.horizontal, 10)
        .background(isSelected ? Color.accentColor.opacity(0.2) : Color.clear)
        .cornerRadius(6)
    }
    
    private var formattedSize: String {
        if file.isFolder {
            return "Folder"
        }
        let sizeKB = Double(file.fileSize) / 1024.0
        if sizeKB > 1024 {
            return String(format: "%.1f MB", sizeKB / 1024.0)
        } else {
            return String(format: "%.0f KB", sizeKB)
        }
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
