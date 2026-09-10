import SwiftUI

/// Shown when the file list has nothing in it.
///
/// A blank pane is ambiguous in the worst way: it looks identical whether the
/// search found nothing, the folder is empty, or the app is broken. Naming the
/// reason costs one view and removes that whole class of doubt.
struct EmptyStateView: View {
    enum Reason {
        case noSearchResults(query: String)
        case emptyFolder(name: String)
        case indexEmpty

        var icon: String {
            switch self {
            case .noSearchResults: return "magnifyingglass"
            case .emptyFolder: return "folder"
            case .indexEmpty: return "clock.arrow.circlepath"
            }
        }

        var title: String {
            switch self {
            case .noSearchResults: return "No matches"
            case .emptyFolder(let name): return "\(name) is empty"
            case .indexEmpty: return "Building the index"
            }
        }

        var detail: String {
            switch self {
            case .noSearchResults(let q):
                return "Nothing matches “\(q)”. Check the filters, or widen the search in Settings → Indexed Folders."
            case .emptyFolder:
                return "There's nothing here yet."
            case .indexEmpty:
                return "Scanning your folders. Recent files will appear as they're found."
            }
        }
    }

    let reason: Reason

    var body: some View {
        VStack(spacing: 8) {
            Image(systemName: reason.icon)
                .font(.system(size: 26, weight: .light))
                .foregroundColor(WarpTheme.textTertiary)
            Text(reason.title)
                .font(.system(size: 14, weight: .medium))
                .foregroundColor(WarpTheme.textSecondary)
            Text(reason.detail)
                .font(WarpTheme.captionFont)
                .foregroundColor(WarpTheme.textTertiary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 320)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(WarpTheme.surfacePrimary)
        .allowsHitTesting(false)
    }
}
