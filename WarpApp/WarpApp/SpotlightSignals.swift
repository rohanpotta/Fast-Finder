import Foundation
import CoreServices

/// Pulls the metadata Spotlight has that the filesystem alone doesn't:
/// when a file *arrived in its folder*, and how much it has been used.
///
/// Two things depend on this. `kMDItemDateAdded` is what Finder's "Date Added"
/// column actually shows — birthtime says when the file was created, which is a
/// different answer for anything moved after creation. And
/// `kMDItemLastUsedDate`/`kMDItemUseCount` seed frecency ranking with usage
/// history the app wasn't around to observe, so search ranks sensibly on day
/// one instead of after weeks of training.
enum SpotlightSignals {

    /// Files per FFI batch. Small enough to keep each write transaction short
    /// so an incremental index update isn't blocked behind a long one.
    private static let batchSize: UInt32 = 500

    /// Upper bound on one pass. Measured at ~850 files/sec, so this caps a pass
    /// at roughly a minute of background work even on a very large index; the
    /// remainder is picked up next launch because the puller is incremental.
    private static let maxPerPass = 50_000

    /// Read Spotlight's view of one file. Attributes are looked up by string
    /// rather than by the exported constants, which aren't uniformly available
    /// to Swift across SDKs.
    private static func read(path: String) -> FileSignal {
        guard let item = MDItemCreate(nil, path as CFString) else {
            return FileSignal(path: path, dateAdded: nil, lastUsedDate: nil, useCount: nil)
        }
        func date(_ attr: String) -> Int64? {
            guard let d = MDItemCopyAttribute(item, attr as CFString) as? Date else { return nil }
            let secs = Int64(d.timeIntervalSince1970)
            // Spotlight occasionally reports a zero/negative date; treat that
            // as "no value" rather than as 1970.
            return secs > 0 ? secs : nil
        }
        return FileSignal(
            path: path,
            dateAdded: date("kMDItemDateAdded"),
            lastUsedDate: date("kMDItemLastUsedDate"),
            useCount: (MDItemCopyAttribute(item, "kMDItemUseCount" as CFString) as? NSNumber)
                .map { Int64(truncating: $0) }
        )
    }

    /// Bring every unpulled file up to date. Safe to call repeatedly: the core
    /// only hands back files it has never asked Spotlight about.
    ///
    /// Returns how many files were updated.
    @discardableResult
    static func runPass() -> Int {
        var total = 0
        while total < maxPerPass {
            let paths = pathsNeedingSignals(limit: batchSize)
            if paths.isEmpty { break }
            let signals = paths.map(read)
            let applied = recordSignals(signals: signals)
            total += Int(applied)
            // Defensive: if the core stops accepting rows we'd otherwise spin
            // on the same batch forever.
            if applied == 0 { break }
        }
        return total
    }
}
