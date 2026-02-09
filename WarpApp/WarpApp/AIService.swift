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
        case query  // Claude sometimes returns "query" instead
    }
    
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        action = try container.decode(AIAction.self, forKey: .action)
        sourcePaths = try container.decodeIfPresent([String].self, forKey: .sourcePaths)
        destination = try container.decodeIfPresent(String.self, forKey: .destination)
        explanation = try container.decode(String.self, forKey: .explanation)
        // Accept either "search_query" or "query"
        searchQuery = try container.decodeIfPresent(String.self, forKey: .searchQuery)
            ?? container.decodeIfPresent(String.self, forKey: .query)
    }
    
    init(action: AIAction, sourcePaths: [String]?, destination: String?, searchQuery: String?, explanation: String) {
        self.action = action
        self.sourcePaths = sourcePaths
        self.destination = destination
        self.searchQuery = searchQuery
        self.explanation = explanation
    }
}

// MARK: - AI Service (Claude Edition)

actor AIService {
    static let shared = AIService()
    
    /// API key: read from Config.xcconfig (copied into app bundle; keep that file gitignored). Fallback: UserDefaults.
    private var apiKey: String? {
        // 1. Config.xcconfig in the app bundle (added to Copy Bundle Resources)
        if let configPath = Bundle.main.path(forResource: "Config", ofType: "xcconfig"),
           let contents = try? String(contentsOfFile: configPath, encoding: .utf8) {
            let key = parseAPIKey(from: contents)
            if let key = key, !key.isEmpty, key != "your-anthropic-api-key-here" {
                return key
            }
        }
        // 2. UserDefaults (e.g. if set in-app later)
        if let stored = UserDefaults.standard.string(forKey: "claude_api_key"), !stored.isEmpty {
            return stored
        }
        return nil
    }
    
    private func parseAPIKey(from xcconfigContents: String) -> String? {
        for line in xcconfigContents.components(separatedBy: .newlines) {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            guard trimmed.hasPrefix("API_KEY") else { continue }
            let parts = trimmed.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
            if parts.count >= 2 {
                return parts[1].trimmingCharacters(in: .whitespaces)
            }
        }
        return nil
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
        
        let fileContext: String
        let contextKind: String
        if !recentSearchResults.isEmpty {
            contextKind = "SEARCH_RESULTS"
            fileContext = recentSearchResults.map { "\($0.filePath) (\($0.fileName))" }.joined(separator: "\n")
        } else if selectedFiles.isEmpty {
            contextKind = "CURRENT_FOLDER"
            fileContext = getFileListingForAi(path: currentFolder)
        } else {
            contextKind = "SELECTED"
            fileContext = selectedFiles.joined(separator: ", ")
        }
        
        let prompt = buildPrompt(
            userQuery: userQuery,
            currentFolder: currentFolder,
            fileContext: fileContext,
            contextKind: contextKind
        )
        
        let response = try await callClaudeAPI(prompt: prompt, apiKey: apiKey)
        return try parseResponse(response)
    }
    
    func setApiKey(_ key: String) {
        UserDefaults.standard.set(key, forKey: "claude_api_key")
    }
    
    func hasApiKey() -> Bool {
        guard let key = apiKey else { return false }
        return !key.isEmpty
    }
    
    // MARK: - Private Helpers
    
    private func buildPrompt(userQuery: String, currentFolder: String, fileContext: String, contextKind: String = "CURRENT_FOLDER") -> String {
        if contextKind == "SEARCH_RESULTS" {
            return buildPromptWithSearchResults(
                userQuery: userQuery,
                currentFolder: currentFolder,
                fileContext: fileContext
            )
        }
        return buildPromptCurrentFolder(
            userQuery: userQuery,
            currentFolder: currentFolder,
            fileContext: fileContext
        )
    }
    
    /// Prompt when we already ran a search and are passing the result list. Model must NOT return search.
    private func buildPromptWithSearchResults(userQuery: String, currentFolder: String, fileContext: String) -> String {
        """
        You are a file operation assistant. Return ONLY a JSON object.
        
        CRITICAL: The app already ran a search. The list below contains ALL search results (one path per line).
        
        DETERMINISM: If the user said "all", "every", or a plural ("screenshots", "pdfs"), you MUST include EVERY matching path in source_paths. One file = only when the user clearly asked for a single item. "Move all screenshots" = every file that is a screenshot (name contains "screenshot" or is an image type).
        
        ALLOWED ACTIONS (only these): moveFiles, trashFiles, compressFiles.
        NOT ALLOWED: search (already done).
        
        SEARCH RESULTS (use these exact paths in source_paths):
        \(fileContext)
        
        USER REQUEST: "\(userQuery)"
        
        INSTRUCTIONS:
        1. Copy the EXACT paths from the list for every file that matches the user's intent. For "all screenshots" = every path that looks like a screenshot (name or type).
        2. source_paths = array of every matching path; do not omit any.
        3. For moveFiles, set destination to a full folder path. If user said "a folder named X", use \(currentFolder)/X.
        
        Return JSON only:
        {"action":"moveFiles","source_paths":["/path/to/file1.png","/path/to/file2.png",...],"destination":"/path/to/folder","explanation":"Moving N files to folder"}
        """
    }
    
    /// Prompt when we're showing current folder listing; search is allowed if no match.
    private func buildPromptCurrentFolder(userQuery: String, currentFolder: String, fileContext: String) -> String {
        """
        You are a file operation assistant. Return ONLY a JSON object.
        
        AVAILABLE ACTIONS:
        - moveFiles: Move files to a destination folder
        - trashFiles: Move files to Trash
        - compressFiles: Create a ZIP archive
        - search: ONLY if no file in the list below matches the user's request (then we will search the system)
        
        CURRENT FOLDER: \(currentFolder)
        
        FILES IN THIS FOLDER (each object has "path", "name", "kind" — use the exact "path" for source_paths):
        \(fileContext)
        
        USER REQUEST: "\(userQuery)"
        
        CRITICAL RULES FOR DETERMINISM:
        1. When the user says "all", "every", or a plural ("screenshots", "pdfs", "the images"), you MUST include EVERY matching file in source_paths. Do not include only one.
        2. Matching: "screenshots" = any file whose name contains "screenshot" or "Screenshot", or kind is image (PNG, JPEG, etc.). "PDFs" = kind PDF or name contains ".pdf". When in doubt, include any file that reasonably matches.
        3. source_paths must be the exact "path" values from the list above (copy them verbatim).
        4. If zero files in the list match, return action "search" with a "query" string so we can search the system.
        5. For moveFiles, destination = full folder path; if user said "a folder named X", use \(currentFolder)/X.
        
        Return JSON only (no markdown):
        {"action":"moveFiles","source_paths":["/exact/path/1","/exact/path/2",...],"destination":"/path/to/folder","explanation":"Moving N files to folder"}
        or {"action":"search","query":"search terms","explanation":"..."}
        """
    }
    
    private func callClaudeAPI(prompt: String, apiKey: String) async throws -> String {
        let url = URL(string: "https://api.anthropic.com/v1/messages")!
        
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue(apiKey, forHTTPHeaderField: "x-api-key")
        request.setValue("2023-06-01", forHTTPHeaderField: "anthropic-version")
        
        let body: [String: Any] = [
            "model": "claude-haiku-4-5-20251001",
            "max_tokens": 1024,
            "messages": [
                ["role": "user", "content": prompt]
            ]
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
        
        // Parse Claude response
        guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let content = json["content"] as? [[String: Any]],
              let firstContent = content.first,
              let text = firstContent["text"] as? String else {
            throw AIError.parseError("Could not parse Claude response")
        }
        
        return text
    }
    
    private func parseResponse(_ response: String) throws -> AIActionPlan {
        var jsonString = response
        
        print("🤖 Raw Claude response: \(response)")
        
        // Strip markdown code blocks
        jsonString = jsonString.replacingOccurrences(of: "```json", with: "")
        jsonString = jsonString.replacingOccurrences(of: "```", with: "")
        jsonString = jsonString.trimmingCharacters(in: .whitespacesAndNewlines)
        
        // Extract JSON between { and }
        guard let jsonStart = jsonString.range(of: "{"),
              let jsonEnd = jsonString.range(of: "}", options: .backwards),
              jsonStart.lowerBound < jsonEnd.upperBound else {
            throw AIError.parseError("No valid JSON found in response: \(response.prefix(200))")
        }
        // Use half-open range to avoid index out of bounds
        jsonString = String(jsonString[jsonStart.lowerBound..<jsonEnd.upperBound])
        
        print("🧹 Cleaned JSON: \(jsonString)")
        
        guard let data = jsonString.data(using: .utf8) else {
            throw AIError.parseError("Could not convert response to data")
        }
        
        do {
            return try JSONDecoder().decode(AIActionPlan.self, from: data)
        } catch {
            throw AIError.parseError("JSON decode failed: \(error.localizedDescription)\nRaw: \(jsonString.prefix(200))")
        }
    }
}

// MARK: - Errors

enum AIError: Error, LocalizedError {
    case noApiKey
    case networkError(String)
    case apiError(String)
    case parseError(String)
    
    var errorDescription: String? {
        switch self {
        case .noApiKey:
            return "No API key configured."
        case .networkError(let msg):
            return "Network error: \(msg)"
        case .apiError(let msg):
            return "API error: \(msg)"
        case .parseError(let msg):
            return "Could not understand AI response: \(msg)"
        }
    }
}
