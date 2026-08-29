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

        return HStack(spacing: 8) {
            Image(systemName: item.icon)
                .font(.system(size: 13))
                .foregroundColor(isSelected ? WarpTheme.accent : WarpTheme.textSecondary)
                .frame(width: 18)
            Text(item.displayName)
                .font(WarpTheme.bodyFont)
                .foregroundColor(isSelected ? WarpTheme.textPrimary : WarpTheme.textSecondary)
            Spacer()
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
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
