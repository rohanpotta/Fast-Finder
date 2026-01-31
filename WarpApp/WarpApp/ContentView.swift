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
            // --- SEARCH BAR AREA ---
            HStack {
                Image(systemName: "magnifyingglass")
                    .foregroundColor(.gray)
                    .font(.title2)
                
                TextField("Search...", text: $query)
                    .textFieldStyle(.plain)
                    .font(.title2)
                    .padding(.vertical, 12)
                    .onSubmit {
                        openSelected()
                    }
                    .onChange(of: query) { newValue in
                        runSearch(for: newValue)
                    }
            }
            .padding(.horizontal)
            // Fix 1: Solid background for search pill to prevent transparent corners
            .background(Color.black.opacity(0.5))
            .cornerRadius(10)
            .padding(12) // Padding from the window edges
            
            Divider()
                .background(Color.gray.opacity(0.3))

            // --- RESULTS LIST ---
            ScrollView {
                LazyVStack(spacing: 0) {
                    ForEach(results, id: \.filePath) { file in
                        HStack {
                            VStack(alignment: .leading) {
                                Text(file.fileName)
                                    .font(.system(size: 14, weight: .medium))
                                    .foregroundColor(.white)
                                Text(file.filePath)
                                    .font(.system(size: 11))
                                    .foregroundColor(.gray)
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                            }
                            Spacer()
                        }
                        .padding(.vertical, 8)
                        .padding(.horizontal, 12)
                        // Selection Highlight Logic
                        .background(selectedId == file.filePath ? Color.blue.opacity(0.4) : Color.clear)
                        .cornerRadius(6)
                        .contentShape(Rectangle()) // Makes the empty space clickable/hoverable
                        
                        // FEATURE: Hover to Select
                        .onHover { isHovering in
                            if isHovering {
                                selectedId = file.filePath
                            }
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
        
        // --- WINDOW BACKGROUND & CORNER FIX ---
        .background(
            ZStack {
                WindowAccessor() // Removes Title Bar
                VisualEffectView(material: .hudWindow, blendingMode: .behindWindow) // Blur
                Color.black.opacity(0.85) // Dark Tint
            }
        )
        // FIX: deeply clips the content + background to the rounded shape
        .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
        // Optional: Adds a thin border for a polished look
        .overlay(
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .stroke(Color.white.opacity(0.15), lineWidth: 1)
        )
        // FIX: Ensures the rounded corners don't get cut off by system margins
        .ignoresSafeArea()
    }
    
    // --- ACTIONS ---
    
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

    func runSearch(for text: String) {
        searchTask?.cancel()
        guard !text.isEmpty else { results = []; return }
        searchTask = Task {
            try? await Task.sleep(nanoseconds: 100_000_000)
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
}

// --- HELPER STRUCTS ---

struct VisualEffectView: NSViewRepresentable {
    let material: NSVisualEffectView.Material
    let blendingMode: NSVisualEffectView.BlendingMode

    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = material
        view.blendingMode = blendingMode
        view.state = .active
        view.appearance = NSAppearance(named: .vibrantDark) // Force Dark Mode
        return view
    }

    func updateNSView(_ view: NSVisualEffectView, context: Context) {
        view.material = material
        view.blendingMode = blendingMode
    }
}

