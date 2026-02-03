//
//  ContentView.swift
//  WarpApp
//
//  Created by Rohan Potta on 1/10/26.
//

//
//  ContentView.swift
//  WarpApp
//
//  Created by Rohan Potta on 1/10/26.
//

import SwiftUI

struct ContentView: View {
    @State private var query = ""
    @State private var results: [SearchResult] = []
    @State private var searchTask: Task<Void, Never>? = nil
    @State private var selectedId: String? = nil

    var body: some View {
            VStack(spacing: 0) {
                // --- SEARCH BAR ---
                HStack {
                    Image(systemName: "magnifyingglass")
                        .foregroundColor(.gray)
                        .font(.title2)
                    
                    TextField("Search...", text: $query)
                        .textFieldStyle(.plain)
                        .font(.title2)
                        .padding(.vertical, 16) // Slightly taller for better look
                        .onSubmit { openSelected() }
                        .onChange(of: query) { newValue in
                            runSearch(for: newValue)
                        }
                }
                .padding(.horizontal)
                // Fix: Ensure this background covers the very top edge
                .background(Color.black.opacity(0.5))
                
                Divider().background(Color.gray.opacity(0.3))

                // --- RESULTS LIST ---
                ScrollView {
                     // ... (Keep your existing List code here) ...
                     LazyVStack(spacing: 0) {
                        ForEach(results, id: \.filePath) { file in
                            FileRowView(
                                file: file,
                                isSelected: selectedId == file.filePath
                            )
                            .contentShape(Rectangle())
                            .onHover { isHovering in
                                if isHovering { selectedId = file.filePath }
                            }
                            .onTapGesture {
                                openFile(file.filePath)
                            }
                        }
                    }
                    .padding(8)
                }
            }
            .frame(minWidth: 600, minHeight: 400)
            .background(
                ZStack {
                    WindowAccessor()
                    VisualEffectView(material: .hudWindow, blendingMode: .behindWindow)
                    Color.black.opacity(0.85)
                }
            )
            // FIX: Remove the manual clipShape if it's cutting off the top
            .cornerRadius(16)
            .overlay(
                RoundedRectangle(cornerRadius: 16, style: .continuous)
                    .stroke(Color.white.opacity(0.15), lineWidth: 1)
            )
            // FIX: This ensures the custom background goes BEHIND the invisible title bar area
            .edgesIgnoringSafeArea(.all)
            .task { loadRecents() }
        }
    
    // --- ACTIONS ---
    
    // Function to fetch recent files
    func loadRecents() {
        Task {
            let recentFiles = await Task.detached(priority: .userInitiated) {
                return getRecentFiles()
            }.value
            
            await MainActor.run {
                // Only populate if the user hasn't started typing yet
                if self.query.isEmpty {
                    self.results = recentFiles
                    self.selectedId = recentFiles.first?.filePath
                }
            }
        }
    }

    func runSearch(for text: String) {
        searchTask?.cancel()
        
        // If query is cleared, go back to showing Recents
        if text.isEmpty {
            loadRecents()
            return
        }
        
        searchTask = Task {
            try? await Task.sleep(nanoseconds: 100_000_000) // Debounce
            if Task.isCancelled { return }
            
            let newResults = await Task.detached(priority: .userInitiated) {
                return searchFiles(query: text)
            }.value
            
            await MainActor.run {
                self.results = newResults
                self.selectedId = newResults.first?.filePath
            }
        }
    }
    
    func openSelected() {
        if let selectedId = selectedId,
           let file = results.first(where: { $0.filePath == selectedId }) {
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
            // Icon
            Image(systemName: file.isFolder ? "folder.fill" : "doc.fill")
                .foregroundColor(file.isFolder ? .blue : .gray)
                .font(.title3)
                .frame(width: 30)

            VStack(alignment: .leading) {
                Text(file.fileName)
                    .font(.system(size: 14, weight: .medium))
                    .foregroundColor(.white)
                
                HStack(spacing: 6) {
                    // Size Text
                    Text(formattedSize)
                        .font(.system(size: 10))
                        .foregroundColor(.gray.opacity(0.8))
                    
                    Text("•")
                        .foregroundColor(.gray.opacity(0.5))
                        .font(.system(size: 8))

                    Text(file.filePath)
                        .font(.system(size: 11))
                        .foregroundColor(.gray)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
            Spacer()
        }
        .padding(.vertical, 8)
        .padding(.horizontal, 12)
        .background(isSelected ? Color.blue.opacity(0.4) : Color.clear)
        .cornerRadius(6)
    }
    
    // Helper property for formatted size
    private var formattedSize: String {
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
        view.appearance = NSAppearance(named: .vibrantDark)
        return view
    }

    func updateNSView(_ view: NSVisualEffectView, context: Context) {
        view.material = material
        view.blendingMode = blendingMode
    }
}
