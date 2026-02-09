import SwiftUI

struct AIInputBar: View {
    @Binding var isPresented: Bool
    @Binding var currentFolder: String
    @Binding var selectedFiles: [String]
    @Binding var prefilledPlan: AIActionPlan?
    
    @State private var query: String = ""
    @State private var isLoading: Bool = false
    @State private var errorMessage: String?
    @State private var actionPlan: AIActionPlan?
    @State private var showConfirmation: Bool = false
    
    /// Called with (plan, userQuery). Parent closes bar when appropriate (e.g. not for search).
    var onExecute: (AIActionPlan, String) -> Void
    
    var body: some View {
        VStack(spacing: 0) {
            // Input field
            HStack(spacing: 12) {
                Image(systemName: "sparkles")
                    .foregroundColor(.purple)
                    .font(.system(size: 18))
                
                TextField("Ask AI to organize your files...", text: $query)
                    .textFieldStyle(.plain)
                    .font(.system(size: 16))
                    .onSubmit {
                        Task { await submitQuery() }
                    }
                
                if isLoading {
                    ProgressView()
                        .scaleEffect(0.7)
                } else if !query.isEmpty {
                    Button(action: { Task { await submitQuery() } }) {
                        Image(systemName: "arrow.right.circle.fill")
                            .foregroundColor(.purple)
                            .font(.system(size: 20))
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            
            Divider()
            
            // Error or result preview
            if let error = errorMessage {
                HStack {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundColor(.orange)
                    Text(error)
                        .foregroundColor(.secondary)
                        .font(.system(size: 13))
                    Spacer()
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 8)
            } else if let plan = actionPlan {
                // Show plan preview
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Image(systemName: iconForAction(plan.action))
                            .foregroundColor(.blue)
                        Text(plan.explanation)
                            .font(.system(size: 13, weight: .medium))
                        Spacer()
                    }
                    
                    if let paths = plan.sourcePaths, !paths.isEmpty {
                        VStack(alignment: .leading, spacing: 4) {
                            ForEach(paths.prefix(5), id: \.self) { path in
                                HStack(spacing: 6) {
                                    Image(systemName: "doc")
                                        .foregroundColor(.secondary)
                                        .font(.system(size: 11))
                                    Text((path as NSString).lastPathComponent)
                                        .font(.system(size: 12))
                                        .foregroundColor(.secondary)
                                }
                            }
                            if paths.count > 5 {
                                Text("...and \(paths.count - 5) more files")
                                    .font(.system(size: 11))
                                    .foregroundColor(.gray.opacity(0.6))
                            }
                        }
                        .padding(.leading, 24)
                    }
                    
                    HStack {
                        Button("Cancel") {
                            actionPlan = nil
                            query = ""
                        }
                        .keyboardShortcut(.escape)
                        
                        Spacer()
                        
                        Button("Execute") {
                            onExecute(plan, query)
                        }
                        .keyboardShortcut(.return)
                        .buttonStyle(.borderedProminent)
                        .tint(.purple)
                    }
                    .padding(.top, 4)
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
            } else {
                // Hints
                VStack(alignment: .leading, spacing: 6) {
                    hintRow(icon: "arrow.right.doc.on.clipboard", text: "\"move resumes to Jobs folder\"")
                    hintRow(icon: "trash", text: "\"delete old downloads\"")
                    hintRow(icon: "archivebox", text: "\"compress screenshots from today\"")
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 10)
            }
        }
        .background(.ultraThinMaterial)
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .shadow(color: .black.opacity(0.2), radius: 20, y: 10)
        .frame(width: 500)
        .onAppear {
            query = ""
            errorMessage = nil
            actionPlan = nil
        }
        .onChange(of: prefilledPlan) { newValue in
            if let plan = newValue {
                actionPlan = plan
                prefilledPlan = nil
            }
        }
    }
    
    private func hintRow(icon: String, text: String) -> some View {
        HStack(spacing: 8) {
            Image(systemName: icon)
                .foregroundColor(.secondary)
                .frame(width: 16)
            Text(text)
                .font(.system(size: 12))
                .foregroundColor(.gray.opacity(0.6))
        }
    }
    
    private func iconForAction(_ action: AIAction) -> String {
        switch action {
        case .moveFiles: return "arrow.right.doc.on.clipboard"
        case .trashFiles: return "trash"
        case .compressFiles: return "archivebox"
        case .search: return "magnifyingglass"
        case .unknown: return "questionmark.circle"
        }
    }
    
    private func submitQuery() async {
        guard !query.isEmpty else { return }
        
        isLoading = true
        errorMessage = nil
        actionPlan = nil
        
        do {
            let plan = try await AIService.shared.parseCommand(
                userQuery: query,
                currentFolder: currentFolder,
                selectedFiles: selectedFiles
            )
            actionPlan = plan
        } catch {
            errorMessage = error.localizedDescription
        }
        
        isLoading = false
    }
}

// MARK: - Preview

#Preview {
    AIInputBar(
        isPresented: .constant(true),
        currentFolder: .constant("/Users/demo/Downloads"),
        selectedFiles: .constant([]),
        prefilledPlan: .constant(nil),
        onExecute: { _, _ in }
    )
    .padding(40)
    .background(Color.gray.opacity(0.3))
}
