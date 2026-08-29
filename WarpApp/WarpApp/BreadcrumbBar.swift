import SwiftUI

struct BreadcrumbBar: View {
    let sidebarItem: SidebarItem
    let navigationStack: [String]
    var onNavigateToIndex: (Int) -> Void  // -1 = root (sidebar item)

    var body: some View {
        HStack(spacing: 4) {
            // Root crumb
            Button(action: { onNavigateToIndex(-1) }) {
                HStack(spacing: 4) {
                    Image(systemName: sidebarItem.icon)
                        .font(.system(size: 11))
                    Text(sidebarItem.displayName)
                        .font(WarpTheme.captionFont)
                }
                .foregroundColor(navigationStack.isEmpty ? WarpTheme.textPrimary : WarpTheme.textSecondary)
            }
            .buttonStyle(.plain)

            // Path crumbs
            ForEach(Array(navigationStack.enumerated()), id: \.offset) { index, path in
                Image(systemName: "chevron.right")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundColor(WarpTheme.textTertiary)

                let name = (path as NSString).lastPathComponent
                let isLast = index == navigationStack.count - 1

                if isLast {
                    Text(name)
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundColor(WarpTheme.textPrimary)
                } else {
                    Button(action: { onNavigateToIndex(index) }) {
                        Text(name)
                            .font(WarpTheme.captionFont)
                            .foregroundColor(WarpTheme.textSecondary)
                    }
                    .buttonStyle(.plain)
                }
            }

            Spacer()
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(WarpTheme.surfacePrimary)
    }
}
