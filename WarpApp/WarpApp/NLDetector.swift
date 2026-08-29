import Foundation

enum NLDetector {
    private static let actionVerbs: Set<String> = [
        "move", "delete", "find", "show", "compress", "organize", "sort",
        "open", "rename", "copy", "create", "clean", "archive", "search",
        "remove", "trash", "list", "group", "zip", "unzip", "extract"
    ]

    private static let questionWords: Set<String> = [
        "where", "which", "what", "how", "when", "why", "who"
    ]

    private static let prepositions: Set<String> = [
        "to", "from", "into", "in", "at", "by", "with", "on", "for"
    ]

    private static let fileExtensionPattern = try! NSRegularExpression(
        pattern: #"(\.\w{1,5}$|\*\.\w{1,5})"#
    )

    static func isNaturalLanguage(_ query: String) -> Bool {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return false }

        let words = trimmed.lowercased()
            .components(separatedBy: .whitespaces)
            .filter { !$0.isEmpty }

        // Single word -> always filename search
        if words.count <= 1 { return false }

        // Short query with file extension pattern -> filename search
        let range = NSRange(trimmed.startIndex..., in: trimmed)
        if fileExtensionPattern.firstMatch(in: trimmed, range: range) != nil && words.count <= 3 {
            return false
        }

        let firstWord = words[0]

        // Starts with action verb -> NL
        if actionVerbs.contains(firstWord) { return true }

        // Starts with question word -> NL
        if questionWords.contains(firstWord) { return true }

        // Contains verb + preposition pattern -> NL
        let hasVerb = words.contains { actionVerbs.contains($0) }
        let hasPrep = words.contains { prepositions.contains($0) }
        if hasVerb && hasPrep { return true }

        // 4+ words with a verb -> NL
        if words.count >= 4 && hasVerb { return true }

        // 5+ words -> NL regardless
        if words.count >= 5 { return true }

        return false
    }
}
