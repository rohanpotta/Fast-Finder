import SwiftUI

enum WarpTheme {
    // MARK: - Backgrounds
    static let background = Color(red: 0.078, green: 0.078, blue: 0.094)        // #141418
    static let surfacePrimary = Color(red: 0.106, green: 0.106, blue: 0.129)     // #1B1B21
    static let surfaceSecondary = Color(red: 0.137, green: 0.137, blue: 0.165)   // #232329
    static let surfaceSelected = Color(red: 0.545, green: 0.361, blue: 0.965).opacity(0.15) // purple tint
    static let surfaceHover = Color.white.opacity(0.05)

    // MARK: - Accent
    static let accent = Color(red: 0.545, green: 0.361, blue: 0.965)            // #8B5CF6
    static let accentGlow = Color(red: 0.545, green: 0.361, blue: 0.965).opacity(0.4)

    // MARK: - Text
    static let textPrimary = Color.white.opacity(0.92)
    static let textSecondary = Color.white.opacity(0.55)
    static let textTertiary = Color.white.opacity(0.35)

    // MARK: - Semantic
    static let destructive = Color(red: 0.937, green: 0.267, blue: 0.267)       // #EF4444
    static let success = Color(red: 0.133, green: 0.773, blue: 0.369)           // #22C55E
    static let warning = Color(red: 0.961, green: 0.620, blue: 0.043)           // #F59E0B

    // MARK: - AI
    static let aiGlow = Color(red: 0.545, green: 0.361, blue: 0.965).opacity(0.3)
    static let aiAccent = Color(red: 0.545, green: 0.361, blue: 0.965)

    // MARK: - Divider
    static let divider = Color.white.opacity(0.08)

    // MARK: - Fonts
    static let titleFont = Font.system(size: 14, weight: .semibold, design: .default)
    static let bodyFont = Font.system(size: 13, weight: .regular, design: .default)
    static let captionFont = Font.system(size: 11, weight: .regular, design: .default)
    static let monoFont = Font.system(size: 12, weight: .regular, design: .monospaced)

    // MARK: - Dimensions
    ///
    /// 28pt rows with 17pt icons. Was 36/20, which is comfortable for a launcher
    /// showing five results and wasteful for a file manager: a file list's job
    /// is to show files, and the taller row cost roughly a quarter of the
    /// visible rows for no added legibility at 13pt text.
    static let fileRowHeight: CGFloat = 28
    static let iconSize: CGFloat = 17
    static let cornerRadius: CGFloat = 8
    static let sidebarWidth: CGFloat = 196
    static let spacing: CGFloat = 8

    /// Height of the single toolbar row (breadcrumb + search + date control).
    static let toolbarHeight: CGFloat = 44
}

// MARK: - AppKit helpers

extension WarpTheme {
    /// Monospaced digits for the size and date columns.
    ///
    /// Proportional figures make a column of numbers ragged because glyph
    /// widths differ; tabular figures line the digits up so the column scans
    /// vertically. This is the single cheapest legibility win in a file list.
    static func columnFont(size: CGFloat = 11) -> NSFont {
        NSFont.monospacedDigitSystemFont(ofSize: size, weight: .regular)
    }
}

// MARK: - NSColor Helpers

extension WarpTheme {
    static let nsBackground = NSColor(red: 0.078, green: 0.078, blue: 0.094, alpha: 1)
    static let nsSurfacePrimary = NSColor(red: 0.106, green: 0.106, blue: 0.129, alpha: 1)
    static let nsSurfaceSecondary = NSColor(red: 0.137, green: 0.137, blue: 0.165, alpha: 1)
    static let nsTextPrimary = NSColor(white: 1, alpha: 0.92)
    static let nsTextSecondary = NSColor(white: 1, alpha: 0.55)
    static let nsTextTertiary = NSColor(white: 1, alpha: 0.35)
    static let nsAccent = NSColor(red: 0.545, green: 0.361, blue: 0.965, alpha: 1)
    static let nsSurfaceSelected = NSColor(red: 0.545, green: 0.361, blue: 0.965, alpha: 0.15)
}
