import SwiftUI

struct StatusBar: View {
    let fileCount: Int
    let selectedCount: Int
    let selectedSize: UInt64

    var body: some View {
        HStack {
            Text("\(fileCount) item\(fileCount == 1 ? "" : "s")")
                .font(WarpTheme.captionFont)
                .foregroundColor(WarpTheme.textTertiary)

            if selectedCount > 0 {
                Text("  |  ")
                    .foregroundColor(WarpTheme.textTertiary)
                Text("\(selectedCount) selected")
                    .font(WarpTheme.captionFont)
                    .foregroundColor(WarpTheme.textSecondary)
                if selectedSize > 0 {
                    Text("(\(formatSize(selectedSize)))")
                        .font(WarpTheme.captionFont)
                        .foregroundColor(WarpTheme.textTertiary)
                }
            }

            Spacer()
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 6)
        .background(WarpTheme.surfacePrimary)
    }

    private func formatSize(_ bytes: UInt64) -> String {
        if bytes < 1024 { return "\(bytes) B" }
        let kb = Double(bytes) / 1024
        if kb < 1024 { return String(format: "%.1f KB", kb) }
        let mb = kb / 1024
        if mb < 1024 { return String(format: "%.1f MB", mb) }
        return String(format: "%.1f GB", mb / 1024)
    }
}
