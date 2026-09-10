import SwiftUI
internal import UniformTypeIdentifiers

struct SidebarView: View {
    @Binding var selectedItem: SidebarItem
    var onDrop: ([NSItemProvider], SidebarItem) -> Bool

    @State private var hoveredItem: SidebarItem?

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            sectionHeader("Favorites")
            ForEach([SidebarItem.recents, .desktop, .documents, .downloads], id: \.self) { item in
                sidebarRow(item)
            }

            Spacer().frame(height: 16)

            sectionHeader("Locations")
            ForEach([SidebarItem.user, .applications, .trash], id: \.self) { item in
                sidebarRow(item)
            }

            Spacer()
        }
        .padding(.top, 12)
        .padding(.horizontal, 8)
        .frame(width: WarpTheme.sidebarWidth)
        .background(WarpTheme.background)
    }

    // MARK: - Components

    private func sectionHeader(_ title: String) -> some View {
        Text(title.uppercased())
            .font(.system(size: 10, weight: .semibold))
            .foregroundColor(WarpTheme.textTertiary)
            .padding(.horizontal, 10)
            .padding(.bottom, 4)
            .padding(.top, 4)
    }

    private func sidebarRow(_ item: SidebarItem) -> some View {
        let isSelected = selectedItem == item
        let isHovered = hoveredItem == item

        return HStack(spacing: 7) {
            Image(systemName: item.icon)
                .font(.system(size: 13))
                // Selected rows read as one object: the icon stops competing
                // with the label for attention once the row itself is tinted.
                .foregroundColor(isSelected ? WarpTheme.textPrimary : WarpTheme.accent.opacity(0.75))
                .frame(width: 17)
            Text(item.displayName)
                .font(.system(size: 13, weight: isSelected ? .medium : .regular))
                .foregroundColor(isSelected ? WarpTheme.textPrimary : WarpTheme.textSecondary)
                .lineLimit(1)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 5)
        .background(
            RoundedRectangle(cornerRadius: 6)
                .fill(isSelected ? WarpTheme.surfaceSelected : (isHovered ? WarpTheme.surfaceHover : Color.clear))
        )
        .contentShape(Rectangle())
        .onHover { hovering in
            hoveredItem = hovering ? item : nil
        }
        .onTapGesture {
            withAnimation(.spring(response: 0.25)) {
                selectedItem = item
            }
        }
        .onDrop(of: [.fileURL], isTargeted: nil) { providers in
            onDrop(providers, item)
        }
    }
}
