import SwiftUI

// MARK: - AI State Machine

enum AIProcessState: Equatable {
    case idle
    case thinking           // Claude is processing
    case searching(String)  // Searching for files
    case planReady          // Plan ready for user review
    case executing          // Action in progress
    case success(String)    // Completed with message
    case error(String)      // Failed with message
}

struct AIInputBar: View {
    @Binding var isPresented: Bool
    @Binding var currentFolder: String
    @Binding var selectedFiles: [String]
    @Binding var prefilledPlan: AIActionPlan?
    
    @State private var query: String = ""
    @State private var state: AIProcessState = .idle
    @State private var actionPlan: AIActionPlan?
    @State private var savedQuery: String = ""
    
    /// Called with (plan, userQuery). Parent handles execution.
    var onExecute: (AIActionPlan, String) -> Void
    
    var body: some View {
        VStack(spacing: 0) {
            // Header with close button
            HStack {
                Image(systemName: "sparkles")
                    .foregroundColor(WarpTheme.aiAccent)
                    .font(.system(size: 16))
                Text("AI Assistant")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundColor(WarpTheme.textPrimary)
                Spacer()
                Button(action: { isPresented = false }) {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundColor(WarpTheme.textSecondary)
                        .font(.system(size: 16))
                }
                .buttonStyle(.plain)
                .keyboardShortcut(.escape)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 10)
            
            Divider()
            
            // Status banner (shows current state)
            statusBanner
            
            // Input field (always visible)
            HStack(spacing: 12) {
                TextField("Describe what you want to do...", text: $query)
                    .textFieldStyle(.plain)
                    .font(.system(size: 15))
                    .foregroundColor(WarpTheme.textPrimary)
                    .disabled(isProcessing)
                    .onSubmit { Task { await submitQuery() } }

                if isProcessing {
                    ProgressView()
                        .scaleEffect(0.7)
                } else if !query.isEmpty && state != .planReady {
                    Button(action: { Task { await submitQuery() } }) {
                        Image(systemName: "arrow.up.circle.fill")
                            .foregroundColor(WarpTheme.aiAccent)
                            .font(.system(size: 22))
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            
            // Content area (plan preview, error, or hints)
            if let plan = actionPlan, state == .planReady {
                planPreview(plan)
            } else if case .idle = state, actionPlan == nil {
                hintsView
            }
        }
        .background(
            RoundedRectangle(cornerRadius: 14)
                .fill(WarpTheme.surfacePrimary)
                .overlay(
                    RoundedRectangle(cornerRadius: 14)
                        .stroke(WarpTheme.divider, lineWidth: 1)
                )
                .shadow(color: WarpTheme.aiGlow, radius: 20, y: 8)
        )
        .clipShape(RoundedRectangle(cornerRadius: 14))
        .shadow(color: .black.opacity(0.4), radius: 25, y: 12)
        .frame(width: 520)
        .onAppear { reset() }
        .onChange(of: prefilledPlan) { newValue in
            if let plan = newValue {
                actionPlan = plan
                state = .planReady
                prefilledPlan = nil
            }
        }
    }
    
    // MARK: - Status Banner
    
    @ViewBuilder
    private var statusBanner: some View {
        switch state {
        case .idle:
            EmptyView()
            
        case .thinking:
            statusRow(icon: "brain.head.profile", iconColor: WarpTheme.aiAccent, text: "Thinking...", isAnimated: true)

        case .searching(let term):
            statusRow(icon: "magnifyingglass", iconColor: WarpTheme.accent, text: "Searching for \"\(term)\"...", isAnimated: true)

        case .planReady:
            EmptyView() // Plan shows in content area

        case .executing:
            statusRow(icon: "gearshape.2", iconColor: WarpTheme.warning, text: "Executing action...", isAnimated: true)

        case .success(let msg):
            HStack(spacing: 10) {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundColor(WarpTheme.success)
                    .font(.system(size: 18))
                Text(msg)
                    .font(.system(size: 13, weight: .medium))
                    .foregroundColor(WarpTheme.textPrimary)
                Spacer()
                Button("Done") { isPresented = false }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 10)
            .background(WarpTheme.success.opacity(0.1))

        case .error(let msg):
            HStack(spacing: 10) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundColor(WarpTheme.warning)
                    .font(.system(size: 16))
                Text(msg)
                    .font(.system(size: 12))
                    .foregroundColor(WarpTheme.textSecondary)
                    .lineLimit(2)
                Spacer()
                Button("Retry") {
                    state = .idle
                    actionPlan = nil
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
            .background(WarpTheme.warning.opacity(0.1))
        }
    }
    
    private func statusRow(icon: String, iconColor: Color, text: String, isAnimated: Bool) -> some View {
        HStack(spacing: 10) {
            Image(systemName: icon)
                .foregroundColor(iconColor)
                .font(.system(size: 14))
                .symbolEffect(.pulse, options: isAnimated ? .repeating : .nonRepeating)
            Text(text)
                .font(.system(size: 13))
                .foregroundColor(WarpTheme.textSecondary)
            Spacer()
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
        .background(iconColor.opacity(0.08))
    }
    
    // MARK: - Plan Preview
    
    private func planPreview(_ plan: AIActionPlan) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Divider()
            
            // Action header with explanation
            HStack(spacing: 10) {
                Image(systemName: iconForAction(plan.action))
                    .foregroundColor(colorForAction(plan.action))
                    .font(.system(size: 20))
                VStack(alignment: .leading, spacing: 2) {
                    Text(titleForAction(plan.action))
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundColor(WarpTheme.textPrimary)
                    Text(plan.explanation)
                        .font(.system(size: 12))
                        .foregroundColor(WarpTheme.textSecondary)
                        .lineLimit(2)
                }
                Spacer()
            }
            .padding(.horizontal, 16)
            .padding(.top, 8)
            
            // File list
            if let paths = plan.sourcePaths, !paths.isEmpty {
                VStack(alignment: .leading, spacing: 3) {
                    ForEach(paths.prefix(4), id: \.self) { path in
                        HStack(spacing: 6) {
                            Image(systemName: "doc.fill")
                                .foregroundColor(WarpTheme.accent.opacity(0.7))
                                .font(.system(size: 10))
                            Text((path as NSString).lastPathComponent)
                                .font(.system(size: 11))
                                .foregroundColor(WarpTheme.textPrimary)
                                .lineLimit(1)
                        }
                    }
                    if paths.count > 4 {
                        Text("+\(paths.count - 4) more files")
                            .font(.system(size: 10))
                            .foregroundColor(WarpTheme.textSecondary)
                    }
                }
                .padding(.horizontal, 46)
            }
            
            // Warn when user said "all" but plan has only one file (common AI slip)
            if let paths = plan.sourcePaths, paths.count <= 1, !query.isEmpty {
                let q = query.lowercased()
                let saidAll = q.contains("all") || q.contains("every") || q.contains("screenshots") || q.contains("pdfs") || q.contains("images")
                if saidAll {
                    HStack(spacing: 6) {
                        Image(systemName: "exclamationmark.triangle.fill")
                            .foregroundColor(.orange)
                            .font(.system(size: 11))
                        Text("Only \(paths.count) file selected. If you meant all matching files, cancel and try again or check the folder.")
                            .font(.system(size: 11))
                            .foregroundColor(.orange)
                    }
                    .padding(.horizontal, 46)
                }
            }
            
            // Destination if moving
            if let dest = plan.destination {
                HStack(spacing: 6) {
                    Image(systemName: "arrow.right")
                        .foregroundColor(WarpTheme.textSecondary)
                        .font(.system(size: 10))
                    Image(systemName: "folder.fill")
                        .foregroundColor(.blue)
                        .font(.system(size: 10))
                    Text((dest as NSString).lastPathComponent)
                        .font(.system(size: 11))
                        .foregroundColor(WarpTheme.textSecondary)
                }
                .padding(.horizontal, 46)
            }
            
            // Action buttons
            HStack {
                Button(action: cancelPlan) {
                    Label("Cancel", systemImage: "xmark")
                }
                .buttonStyle(.bordered)
                .controlSize(.regular)
                
                Spacer()
                
                Button(action: executePlan) {
                    Label(executeButtonTitle(plan.action), systemImage: "play.fill")
                }
                .buttonStyle(.borderedProminent)
                .tint(colorForAction(plan.action))
                .controlSize(.regular)
                .keyboardShortcut(.return)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 10)
        }
    }
    
    // MARK: - Hints View
    
    private var hintsView: some View {
        VStack(alignment: .leading, spacing: 5) {
            Divider()
            Text("Try saying:")
                .font(.system(size: 11))
                .foregroundColor(WarpTheme.textSecondary)
                .padding(.horizontal, 16)
                .padding(.top, 6)
            
            VStack(alignment: .leading, spacing: 4) {
                hintRow(icon: "arrow.right.doc.on.clipboard", text: "\"move resumes to Jobs folder\"")
                hintRow(icon: "trash", text: "\"delete old screenshots\"")
                hintRow(icon: "archivebox", text: "\"compress all PDFs here\"")
            }
            .padding(.horizontal, 16)
            .padding(.bottom, 10)
        }
    }
    
    private func hintRow(icon: String, text: String) -> some View {
        HStack(spacing: 8) {
            Image(systemName: icon)
                .foregroundColor(WarpTheme.textTertiary)
                .frame(width: 14)
                .font(.system(size: 11))
            Text(text)
                .font(.system(size: 11))
                .foregroundColor(WarpTheme.textSecondary)
        }
    }
    
    // MARK: - Helpers
    
    private var isProcessing: Bool {
        switch state {
        case .thinking, .searching, .executing: return true
        default: return false
        }
    }
    
    private func reset() {
        query = ""
        state = .idle
        actionPlan = nil
        savedQuery = ""
    }
    
    private func cancelPlan() {
        state = .idle
        actionPlan = nil
    }
    
    private func executePlan() {
        guard let plan = actionPlan else { return }
        state = .executing
        onExecute(plan, savedQuery)
        
        // Show success after a brief delay (parent handles actual execution)
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
            state = .success(successMessage(for: plan))
        }
    }
    
    private func submitQuery() async {
        guard !query.isEmpty else { return }
        savedQuery = query
        state = .thinking
        actionPlan = nil
        
        do {
            let plan = try await AIService.shared.parseCommand(
                userQuery: query,
                currentFolder: currentFolder,
                selectedFiles: selectedFiles
            )
            
            if plan.action == .search {
                // AI wants to search first
                state = .searching(plan.searchQuery ?? query)
                // Execute search (parent will handle via onExecute)
                onExecute(plan, savedQuery)
                return
            }
            
            actionPlan = plan
            state = .planReady
        } catch {
            state = .error(error.localizedDescription)
        }
    }
    
    private func iconForAction(_ action: AIAction) -> String {
        switch action {
        case .moveFiles: return "arrow.right.doc.on.clipboard"
        case .trashFiles: return "trash.fill"
        case .compressFiles: return "archivebox.fill"
        case .search: return "magnifyingglass"
        case .unknown: return "questionmark.circle"
        }
    }
    
    private func colorForAction(_ action: AIAction) -> Color {
        switch action {
        case .moveFiles: return WarpTheme.accent
        case .trashFiles: return WarpTheme.destructive
        case .compressFiles: return WarpTheme.warning
        case .search: return WarpTheme.aiAccent
        case .unknown: return WarpTheme.textSecondary
        }
    }
    
    private func titleForAction(_ action: AIAction) -> String {
        switch action {
        case .moveFiles: return "Move Files"
        case .trashFiles: return "Move to Trash"
        case .compressFiles: return "Compress Files"
        case .search: return "Search"
        case .unknown: return "Unknown"
        }
    }
    
    private func executeButtonTitle(_ action: AIAction) -> String {
        switch action {
        case .moveFiles: return "Move"
        case .trashFiles: return "Trash"
        case .compressFiles: return "Compress"
        case .search: return "Search"
        case .unknown: return "Execute"
        }
    }
    
    private func successMessage(for plan: AIActionPlan) -> String {
        let count = plan.sourcePaths?.count ?? 0
        switch plan.action {
        case .moveFiles: return "Moved \(count) file\(count == 1 ? "" : "s")"
        case .trashFiles: return "Trashed \(count) file\(count == 1 ? "" : "s")"
        case .compressFiles: return "Created archive with \(count) file\(count == 1 ? "" : "s")"
        case .search: return "Search complete"
        case .unknown: return "Done"
        }
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
    .background(WarpTheme.background)
}
