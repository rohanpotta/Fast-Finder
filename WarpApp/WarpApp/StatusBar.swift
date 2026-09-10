import SwiftUI

struct StatusBar: View {
    let fileCount: Int
    let selectedCount: Int
    let selectedSize: UInt64

    var body: some View {
        HStack(spacing: 8) {
            Text("\(fileCount) item\(fileCount == 1 ? "" : "s")")
                .font(WarpTheme.captionFont)
                .foregroundColor(WarpTheme.textTertiary)

            if selectedCount > 0 {
                // A drawn separator rather than "  |  " as text, which
                // inherited the surrounding font and never sat on the baseline.
                Rectangle()
                    .fill(WarpTheme.divider)
                    .frame(width: 1, height: 9)

                Text("\(selectedCount) selected")
                    .font(WarpTheme.captionFont)
                    .foregroundColor(WarpTheme.textSecondary)
                if selectedSize > 0 {
                    Text(formatSize(selectedSize))
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundColor(WarpTheme.textTertiary)
                }
            }

            Spacer()
        }
        .padding(.horizontal, 12)
        .frame(height: 22)
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
