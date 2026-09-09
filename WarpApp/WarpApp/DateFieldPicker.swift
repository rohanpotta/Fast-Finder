import SwiftUI

/// Which date drives sorting, filtering and the date column.
///
/// A local mirror of the Rust `DateField` so it can be `Codable`/`RawRepresentable`
/// for `@AppStorage` — the generated UniFFI enum is neither.
enum DateFieldChoice: String, CaseIterable, Identifiable {
    case either
    case added
    case modified

    var id: String { rawValue }

    /// Label for the picker and the date column header.
    var label: String {
        switch self {
        case .either: return "Date"
        case .added: return "Date Added"
        case .modified: return "Date Modified"
        }
    }

    /// Short form for the picker button, which sits in a tight row.
    var shortLabel: String {
        switch self {
        case .either: return "Recent"
        case .added: return "Added"
        case .modified: return "Modified"
        }
    }

    var help: String {
        switch self {
        case .either: return "Whichever is newer — added or modified"
        case .added: return "When the file was created on this Mac"
        case .modified: return "When the file's contents last changed"
        }
    }

    var icon: String {
        switch self {
        case .either: return "clock"
        case .added: return "tray.and.arrow.down"
        case .modified: return "pencil"
        }
    }

    var ffi: DateField {
        switch self {
        case .either: return .either
        case .added: return .added
        case .modified: return .modified
        }
    }
}

/// Compact menu for forcing which date the list is organised by.
struct DateFieldPicker: View {
    @Binding var choice: DateFieldChoice

    var body: some View {
        Menu {
            ForEach(DateFieldChoice.allCases) { option in
                Button {
                    choice = option
                } label: {
                    // A checkmark rather than a Picker so the help text fits.
                    HStack {
                        Image(systemName: option.icon)
                        VStack(alignment: .leading) {
                            Text(option.label)
                            Text(option.help)
                                .font(.caption)
                        }
                        if choice == option {
                            Image(systemName: "checkmark")
                        }
                    }
                }
            }
        } label: {
            HStack(spacing: 5) {
                Image(systemName: choice.icon)
                    .font(.system(size: 11))
                Text(choice.shortLabel)
                    .font(.system(size: 12, weight: .medium))
                Image(systemName: "chevron.down")
                    .font(.system(size: 8, weight: .semibold))
            }
            .foregroundColor(choice == .either ? WarpTheme.textSecondary : WarpTheme.accent)
            .padding(.horizontal, 9)
            .padding(.vertical, 6)
            .background(
                RoundedRectangle(cornerRadius: 8)
                    .fill(choice == .either ? WarpTheme.surfaceSecondary : WarpTheme.accent.opacity(0.12))
                    .overlay(
                        RoundedRectangle(cornerRadius: 8)
                            .stroke(
                                choice == .either ? WarpTheme.divider : WarpTheme.accent.opacity(0.4),
                                lineWidth: 1
                            )
                    )
            )
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .help("Choose which date organises the list")
    }
}
