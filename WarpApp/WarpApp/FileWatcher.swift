import Foundation
import Combine
import CoreServices

/// One coalesced batch of filesystem changes, as reported by FSEvents.
struct FileChangeBatch {
    /// Changed paths. Directories are passed through as-is — the Rust indexer
    /// re-walks a directory subtree when it sees one, which is also how a
    /// coalesced `MustScanSubDirs` event gets resolved.
    let paths: [String]

    /// A watched root itself moved, or a volume came or went. The incremental
    /// stream can't be trusted across that, so the caller should fall back to
    /// a full rescan.
    let needsFullRescan: Bool

    /// Highest event id in this batch. Persist it only *after* the batch has
    /// been folded into the index, so a crash mid-batch replays it rather than
    /// skipping it.
    let latestEventId: UInt64
}

class FileWatcher: ObservableObject {
    private var stream: FSEventStreamRef?
    private let paths: [String]
    var onChange: ((FileChangeBatch) -> Void)?

    init(paths: [String]) {
        self.paths = paths
    }

    deinit {
        stop()
    }

    /// - Parameter sinceEventId: resume point from a previous run, so a
    ///   relaunch replays what changed while the app was closed instead of
    ///   forcing a full re-walk. Pass 0 to only watch from now on.
    func start(sinceEventId: UInt64 = 0) {
        guard stream == nil else { return }

        let cfPaths = paths as CFArray
        var context = FSEventStreamContext()
        context.info = Unmanaged.passUnretained(self).toOpaque()

        let since = sinceEventId == 0
            ? FSEventStreamEventId(kFSEventStreamEventIdSinceNow)
            : FSEventStreamEventId(sinceEventId)

        let callback: FSEventStreamCallback = { _, clientCallBackInfo, numEvents, eventPaths, eventFlags, eventIds in
            guard let info = clientCallBackInfo else { return }
            let watcher = Unmanaged<FileWatcher>.fromOpaque(info).takeUnretainedValue()

            // kFSEventStreamCreateFlagUseCFTypes means eventPaths is a
            // CFArray of CFString rather than a C string array.
            let reported = unsafeBitCast(eventPaths, to: NSArray.self) as? [String] ?? []

            var changed: [String] = []
            var seen = Set<String>()
            var mustRescan = false
            var latest: UInt64 = 0

            for i in 0..<numEvents {
                let flags = eventFlags[i]
                let id = UInt64(eventIds[i])
                if id != 0 { latest = max(latest, id) }

                // Sentinel marking the end of replayed history; carries no path.
                if flags & UInt32(kFSEventStreamEventFlagHistoryDone) != 0 { continue }

                // A watched root moved, or a volume appeared/disappeared —
                // targeted updates can't express that.
                if flags & UInt32(kFSEventStreamEventFlagRootChanged) != 0
                    || flags & UInt32(kFSEventStreamEventFlagMount) != 0
                    || flags & UInt32(kFSEventStreamEventFlagUnmount) != 0 {
                    mustRescan = true
                    continue
                }

                guard i < reported.count else { continue }
                let path = reported[i]
                // FSEvents repeats a path when several things happened to it
                // inside one latency window; indexing it once is enough.
                if seen.insert(path).inserted {
                    changed.append(path)
                }
            }

            guard !changed.isEmpty || mustRescan else { return }

            let batch = FileChangeBatch(
                paths: changed,
                needsFullRescan: mustRescan,
                latestEventId: latest
            )
            DispatchQueue.main.async {
                watcher.onChange?(batch)
            }
        }

        stream = FSEventStreamCreate(
            nil,
            callback,
            &context,
            cfPaths,
            since,
            1.0,  // 1-second latency debounce
            UInt32(
                kFSEventStreamCreateFlagUseCFTypes
                | kFSEventStreamCreateFlagFileEvents
                // Deliver the first event of a burst immediately instead of
                // waiting out the full latency window.
                | kFSEventStreamCreateFlagNoDefer
                // Required for RootChanged, i.e. someone moved ~/Documents.
                | kFSEventStreamCreateFlagWatchRoot
            )
        )

        if let stream = stream {
            FSEventStreamSetDispatchQueue(stream, DispatchQueue.main)
            FSEventStreamStart(stream)
        }
    }

    func stop() {
        if let stream = stream {
            FSEventStreamStop(stream)
            // When using FSEventStreamSetDispatchQueue, unset the queue before invalidating
            FSEventStreamSetDispatchQueue(stream, nil)
            FSEventStreamInvalidate(stream)
            FSEventStreamRelease(stream)
        }
        stream = nil
    }
}
