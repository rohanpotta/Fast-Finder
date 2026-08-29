import SwiftUI

struct SearchBarView: View {
    @Binding var query: String
    var isNLDetected: Bool
    var onSubmit: () -> Void
    @FocusState private var isFocused: Bool

    var body: some View {
        HStack(spacing: 10) {
            // Icon: magnifying glass or sparkles for NL
            Group {
                if isNLDetected {
                    Image(systemName: "sparkles")
                        .foregroundColor(WarpTheme.accent)
                } else {
                    Image(systemName: "magnifyingglass")
                        .foregroundColor(WarpTheme.textTertiary)
                }
            }
            .font(.system(size: 16))
            .animation(.easeInOut(duration: 0.2), value: isNLDetected)

            TextField("Search files or ask AI...", text: $query)
                .textFieldStyle(.plain)
                .font(.system(size: 15))
                .foregroundColor(WarpTheme.textPrimary)
                .focused($isFocused)
                .onSubmit { onSubmit() }

            if isNLDetected {
                Text("AI")
                    .font(.system(size: 10, weight: .bold))
                    .foregroundColor(WarpTheme.accent)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(WarpTheme.accent.opacity(0.15))
                    .clipShape(Capsule())
                    .transition(.scale.combined(with: .opacity))
            }

            if !query.isEmpty {
                Button(action: { query = "" }) {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundColor(WarpTheme.textTertiary)
                        .font(.system(size: 14))
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(
            RoundedRectangle(cornerRadius: 12)
                .fill(WarpTheme.surfaceSecondary)
                .overlay(
                    RoundedRectangle(cornerRadius: 12)
                        .stroke(
                            isFocused ? WarpTheme.accent.opacity(0.6) : WarpTheme.divider,
                            lineWidth: isFocused ? 1.5 : 1
                        )
                )
                .shadow(color: isFocused ? WarpTheme.accentGlow : .clear, radius: 8)
        )
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .animation(.easeInOut(duration: 0.2), value: isFocused)
        .animation(.easeInOut(duration: 0.2), value: isNLDetected)
    }
}
