import SwiftUI

/// Shows how the search bar's text was understood: one chip per recognised
/// filter, plus a warning for tokens that looked like filters but weren't.
///
/// Without this the syntax is invisible — you'd type `added:<7d`, get results,
/// and have no way to tell whether the filter applied or whether it was
/// silently searched as literal text.
struct FilterChipsBar: View {
    let parsed: ParsedQuery
    /// Remove one filter token from the query.
    var onRemove: (String) -> Void

    var body: some View {
        // Nothing to say when no filters are in play — the row stays absent
        // rather than becoming permanent chrome. Discoverability lives in the
        // search field's placeholder instead.
        if !parsed.chips.isEmpty || !parsed.invalid.isEmpty {
            HStack(spacing: 6) {
                ForEach(parsed.chips, id: \.token) { chip in
                    chipView(chip)
                }
                ForEach(parsed.invalid, id: \.self) { token in
                    invalidView(token)
                }
                Spacer()
                if !parsed.text.isEmpty {
                    Text("matching “\(parsed.text)”")
                        .font(.system(size: 11))
                        .foregroundColor(WarpTheme.textTertiary)
                }
            }
            .padding(.horizontal, 14)
            .padding(.bottom, 8)
        }
    }

    private func chipView(_ chip: QueryChip) -> some View {
        HStack(spacing: 4) {
            Text(chip.label)
                .font(.system(size: 11, weight: .medium))
                .foregroundColor(WarpTheme.accent)
            Button {
                onRemove(chip.token)
            } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 7, weight: .bold))
                    .foregroundColor(WarpTheme.accent.opacity(0.8))
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, 7)
        .padding(.vertical, 3)
        .background(
            Capsule()
                .fill(WarpTheme.accent.opacity(0.14))
                .overlay(Capsule().stroke(WarpTheme.accent.opacity(0.35), lineWidth: 1))
        )
        .help("Filter: \(chip.label)")
    }

    private func invalidView(_ token: String) -> some View {
        HStack(spacing: 4) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 8))
            Text(token)
                .font(.system(size: 11, weight: .medium))
        }
        .foregroundColor(WarpTheme.warning)
        .padding(.horizontal, 7)
        .padding(.vertical, 3)
        .background(
            Capsule()
                .fill(WarpTheme.warning.opacity(0.12))
                .overlay(Capsule().stroke(WarpTheme.warning.opacity(0.35), lineWidth: 1))
        )
        .help("Not a filter this app understands — searched as plain text instead")
    }
}
