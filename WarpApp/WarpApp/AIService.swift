import Foundation

// MARK: - AI Action Plan Models

enum AIAction: String, Codable {
    case moveFiles
    case trashFiles
    case compressFiles
    case search
    case unknown
}

struct AIActionPlan: Decodable, Equatable {
    let action: AIAction
    let sourcePaths: [String]?
    let destination: String?
    let searchQuery: String?
    let explanation: String

    enum CodingKeys: String, CodingKey {
        case action, explanation, destination
        case sourcePaths = "source_paths"
        case searchQuery = "search_query"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        action = try container.decode(AIAction.self, forKey: .action)
        sourcePaths = try container.decodeIfPresent([String].self, forKey: .sourcePaths)
        destination = try container.decodeIfPresent(String.self, forKey: .destination)
        searchQuery = try container.decodeIfPresent(String.self, forKey: .searchQuery)
        explanation = try container.decode(String.self, forKey: .explanation)
    }

    init(action: AIAction, sourcePaths: [String]?, destination: String?, searchQuery: String?, explanation: String) {
        self.action = action
        self.sourcePaths = sourcePaths
        self.destination = destination
        self.searchQuery = searchQuery
        self.explanation = explanation
    }
}

// MARK: - AI Service (Claude tool use)

actor AIService {
    static let shared = AIService()

    /// Anthropic Sonnet — current cheap+fast model for structured tool calls.
    /// If you change this, also re-test the tool-use response parser.
    private let model = "claude-sonnet-4-6"

    /// Run on app launch (from WarpAppApp.init). Moves any legacy plaintext
    /// key out of UserDefaults into the Keychain and then deletes the
    /// UserDefaults copy. Idempotent: safe to call every launch.
    nonisolated static func migrateAPIKeyOnLaunch() {
        let defaults = UserDefaults.standard
        if let legacy = defaults.string(forKey: "claude_api_key"), !legacy.isEmpty {
            if KeychainHelper.load() == nil {
                _ = KeychainHelper.save(apiKey: legacy)
            }
            defaults.removeObject(forKey: "claude_api_key")
        }
    }

    /// API key: Keychain only.
    ///
    /// There used to be a fallback that read `Config.xcconfig` out of the app
    /// bundle. That file was in Copy Bundle Resources, so the key shipped
    /// inside `WarpApp.app/Contents/Resources/` in plaintext — readable by
    /// anyone handed a build. Gitignoring it protected the repo, not the
    /// binary. Keychain is now the only source; Settings is where you set it.
    private var apiKey: String? {
        guard let key = KeychainHelper.load(), !key.isEmpty else { return nil }
        return key
    }

    // MARK: - Public API

    func parseCommand(
        userQuery: String,
        currentFolder: String,
        selectedFiles: [String],
        recentSearchResults: [SearchResult] = []
    ) async throws -> AIActionPlan {
        guard let apiKey = apiKey, !apiKey.isEmpty else {
            throw AIError.noApiKey
        }

        // Build BOTH the prompt context AND the allow-list of paths the model
        // is permitted to act on. The allow-list is what saves us from prompt
        // injection via malicious filenames — even if a file is named
        // `; rm -rf ~`, the worst it can do is fail to be re-quoted later;
        // it can't smuggle /etc/passwd into source_paths because that string
        // is not in the allow-list.
        let allowedPaths: Set<String>
        let fileContext: String
        let contextKind: String

        if !recentSearchResults.isEmpty {
            contextKind = "SEARCH_RESULTS"
            allowedPaths = Set(recentSearchResults.map { $0.filePath })
            fileContext = recentSearchResults.map { "\($0.filePath) (\($0.fileName))" }.joined(separator: "\n")
        } else if selectedFiles.isEmpty {
            contextKind = "CURRENT_FOLDER"
            let listing = getFileListingForAi(path: currentFolder)
            allowedPaths = Self.extractPathsFromListing(listing)
            fileContext = listing
        } else {
            contextKind = "SELECTED"
            allowedPaths = Set(selectedFiles)
            fileContext = selectedFiles.joined(separator: ", ")
        }

        let prompt = buildPrompt(
            userQuery: userQuery,
            currentFolder: currentFolder,
            fileContext: fileContext,
            contextKind: contextKind
        )

        let plan = try await callClaudeWithTools(prompt: prompt, apiKey: apiKey)

        // Prompt-injection defense: every source_path must be one we showed
        // the model. Destinations are NOT validated here — they can be new
        // folders the user asked for. The Rust `safety` layer enforces the
        // policy for destinations.
        if let sources = plan.sourcePaths, !sources.isEmpty {
            let unknown = sources.filter { !allowedPaths.contains($0) }
            if !unknown.isEmpty {
                throw AIError.injectionDetected(unknown)
            }
        }

        return plan
    }

    func setApiKey(_ key: String) {
        // Keychain-only. We never write to UserDefaults — see
        // migrateAPIKeyOnLaunch for the migration path.
        _ = KeychainHelper.save(apiKey: key)
    }

    func hasApiKey() -> Bool {
        guard let key = apiKey else { return false }
        return !key.isEmpty
    }

    // MARK: - Private Helpers

    /// Extract `"path": "..."` values from the JSON returned by
    /// `getFileListingForAi`. Tolerates missing/malformed JSON by returning
    /// an empty set (the caller will then reject any source_paths the model
    /// returns, which is the correct safe default).
    private static func extractPathsFromListing(_ listing: String) -> Set<String> {
        guard let data = listing.data(using: .utf8),
              let arr = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]] else {
            return []
        }
        return Set(arr.compactMap { $0["path"] as? String })
    }

    private func buildPrompt(userQuery: String, currentFolder: String, fileContext: String, contextKind: String) -> String {
        if contextKind == "SEARCH_RESULTS" {
            return """
            The app already ran a search. The list below contains the full result set
            (one path per line). When the user said "all", "every", or used a plural
            ("screenshots", "pdfs"), include EVERY matching path in source_paths.

            ALLOWED ACTIONS: moveFiles, trashFiles, compressFiles. (search is forbidden — already ran.)

            SEARCH RESULTS:
            \(fileContext)

            USER REQUEST: "\(userQuery)"

            For moveFiles, set destination to a full folder path. If the user said
            "a folder named X", use \(currentFolder)/X.
            """
        }
        return """
        AVAILABLE ACTIONS:
        - moveFiles: move files to a destination folder
        - trashFiles: send to Trash
        - compressFiles: build a ZIP archive
        - search: ONLY if no file in the list below matches the user's intent

        CURRENT FOLDER: \(currentFolder)

        FILES IN THIS FOLDER (JSON with "path", "name", "kind"; use exact paths):
        \(fileContext)

        USER REQUEST: "\(userQuery)"

        When the user says "all", "every", or a plural ("screenshots", "pdfs"),
        include EVERY matching file in source_paths. Match generously:
        "screenshots" = any file whose name contains "screenshot" or whose kind is
        an image; "PDFs" = kind PDF or name ending .pdf. For moveFiles, set
        destination to a full folder path. If the user said "a folder named X",
        use \(currentFolder)/X. If nothing in the list matches, return search with
        a search_query.
        """
    }

    private func callClaudeWithTools(prompt: String, apiKey: String) async throws -> AIActionPlan {
        let url = URL(string: "https://api.anthropic.com/v1/messages")!
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue(apiKey, forHTTPHeaderField: "x-api-key")
        request.setValue("2023-06-01", forHTTPHeaderField: "anthropic-version")

        // Structured output via tool use: we force the model to call this tool,
        // which means the response is always a typed JSON object — no more
        // regex-extracting `{...}` from free-form prose.
        let toolName = "plan_file_action"
        let toolDef: [String: Any] = [
            "name": toolName,
            "description": "Return the file management action plan based on the user request and provided file context.",
            "input_schema": [
                "type": "object",
                "properties": [
                    "action": [
                        "type": "string",
                        "enum": ["moveFiles", "trashFiles", "compressFiles", "search"],
                        "description": "Which operation to perform."
                    ],
                    "source_paths": [
                        "type": "array",
                        "items": ["type": "string"],
                        "description": "Absolute file paths copied verbatim from the provided context. Required for moveFiles, trashFiles, compressFiles."
                    ],
                    "destination": [
                        "type": "string",
                        "description": "Absolute folder path. Required for moveFiles and the archive path for compressFiles."
                    ],
                    "search_query": [
                        "type": "string",
                        "description": "Free-text search query. Required for the search action only."
                    ],
                    "explanation": [
                        "type": "string",
                        "description": "Short human-readable summary the UI will display."
                    ]
                ],
                "required": ["action", "explanation"]
            ]
        ]

        let body: [String: Any] = [
            "model": model,
            "max_tokens": 1024,
            "tools": [toolDef],
            "tool_choice": ["type": "tool", "name": toolName],
            "messages": [["role": "user", "content": prompt]]
        ]
        request.httpBody = try JSONSerialization.data(withJSONObject: body)

        let (data, response) = try await URLSession.shared.data(for: request)
        guard let httpResponse = response as? HTTPURLResponse else {
            throw AIError.networkError("Invalid response")
        }
        guard httpResponse.statusCode == 200 else {
            let errorText = String(data: data, encoding: .utf8) ?? "Unknown error"
            throw AIError.apiError("Status \(httpResponse.statusCode): \(errorText)")
        }

        guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let content = json["content"] as? [[String: Any]] else {
            throw AIError.parseError("Malformed Claude response shape")
        }

        // Find the tool_use block. The API may emit a leading thinking/text
        // block alongside the tool_use; we tolerate any ordering.
        guard let toolUseBlock = content.first(where: { ($0["type"] as? String) == "tool_use" }),
              let input = toolUseBlock["input"] as? [String: Any] else {
            throw AIError.parseError("Claude did not return a tool_use block")
        }

        // Re-encode the typed input dict and decode through Codable so the
        // existing AIActionPlan validation logic stays in one place.
        let inputData = try JSONSerialization.data(withJSONObject: input)
        do {
            return try JSONDecoder().decode(AIActionPlan.self, from: inputData)
        } catch {
            throw AIError.parseError("tool_use input did not match AIActionPlan: \(error.localizedDescription)")
        }
    }
}

// MARK: - Errors

enum AIError: Error, LocalizedError {
    case noApiKey
    case networkError(String)
    case apiError(String)
    case parseError(String)
    case injectionDetected([String])

    var errorDescription: String? {
        switch self {
        case .noApiKey:
            return "No API key configured. Add one in Settings (⌘,) → AI Assistant."
        case .networkError(let msg):
            return "Network error: \(msg)"
        case .apiError(let msg):
            return "API error: \(msg)"
        case .parseError(let msg):
            return "Could not understand AI response: \(msg)"
        case .injectionDetected(let unknown):
            let preview = unknown.prefix(3).joined(separator: ", ")
            let more = unknown.count > 3 ? " (+\(unknown.count - 3) more)" : ""
            return "Refused: AI proposed paths that weren't in the shown file list — \(preview)\(more)"
        }
    }
}
